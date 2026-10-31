# -*- coding: utf-8 -*-
"""调色台模块：图库 / 显影 / 导出三个子模式，面板可停靠也可浮动。

窗口本身（菜单栏、模块栏、状态栏进度与取消）归外壳所有，这里只交出一块预览页
和四个停靠面板。子模式切换只改本页与面板显隐，不换模块。
素材从外壳的 Material 流进来：合成写完视频会自动载入预览。
"""
import copy
import json
import os
import re
import shutil
import tempfile
import time

import numpy as np
from PySide6 import QtCore, QtGui, QtWidgets

from reference import deflicker as flicker
from reference import ff as ffmod
from reference import grade as gc
from reference import holy as holly
from reference import imio
from reference import qa as qa_mod
from reference import sequence as seq
from reference import video
from reference import xmp
from reference.gui import (edits, hints, icons, job as jobs, playback, prefs,
                       startpage, theme, widgets)
from reference.gui.assemble import (ASPECT_CHOICES, CODEC_CHOICES, FPS_CHOICES,
                                SIDE_CHOICES)
from reference.gui.grader import exporter, filmstrip, imageview, panels
from reference.gui.grader import deflicker as flicker_ui
from reference.gui.grader import holy as holy_ui
from reference.gui.grader import params, pipeline, ratings, renderq, scopes
from reference.gui.grader import qa as qa_ui
from reference.gui.module import Module

SUBMODES = ("图库", "显影", "导出")
IMG_EXT = (tuple(imio.IMG_EXTENSIONS) + (".bmp", ".webp", ".gif")
           + tuple(sorted(imio.RAW_EXTENSIONS)))
VID_EXT = (".mp4", ".mov", ".mkv", ".avi", ".m4v", ".ts", ".mts", ".m2ts",
           ".webm", ".wmv", ".hevc", ".h264")
SUB_HINT = {"图库": "按 1~5 打星、0 清除，右键选色标；双击缩略图进入显影",
            "显影": "拖动滑块调色，Ctrl+\\ 对比，Ctrl+Shift+R 参考帧",
            "导出": "选格式与质量，导出用状态栏那一条全局进度"}
SUB_ICONS = {"图库": "grid", "显影": "sun", "导出": "download"}
# 图库用中央整窗网格，显影要滑块/示波器/胶片条，导出只要导出面板
SUB_DOCKS = {"图库": (),
             "显影": ("dock_tools", "dock_scopes", "dock_film"),
             "导出": ("dock_export",)}


class _Page(QtWidgets.QWidget):
    """调色台中央页：子控件不吃掉的按键转给播放条。

    不能用 installEventFilter(self)——PySide6 6.12 里给 QWidget 装完
    Python 侧过滤器，之后同对象的 QVBoxLayout() 会直接返回 NULL。
    """

    def __init__(self, owner):
        super().__init__()
        self.owner = owner

    def keyPressEvent(self, ev):
        if not self.owner.page_key(ev.key()):
            super().keyPressEvent(ev)


SHEET_CELL = 380             # 联系表每半格的宽（像素）：左右各一格 + 一条分隔线


class TimeSlider(QtWidgets.QSlider):
    """时间轴：在关键帧的帧号位置画小菱形，不用另外开一条标记栏。"""

    def __init__(self, parent=None):
        super().__init__(QtCore.Qt.Orientation.Horizontal, parent)
        self.marks = []

    def paintEvent(self, e):
        super().paintEvent(e)
        if not self.marks or self.maximum() <= self.minimum():
            return
        opt = QtWidgets.QStyleOptionSlider()
        self.initStyleOption(opt)
        groove = self.style().subControlRect(
            QtWidgets.QStyle.ComplexControl.CC_Slider, opt,
            QtWidgets.QStyle.SubControl.SC_SliderGroove, self)
        if groove.width() <= 0:
            return
        p = QtGui.QPainter(self)
        p.setRenderHint(QtGui.QPainter.RenderHint.Antialiasing)
        p.setPen(QtCore.Qt.PenStyle.NoPen)
        p.setBrush(QtGui.QColor(theme.ACCENT))
        y = float(groove.center().y())
        for i in self.marks:
            if not self.minimum() <= i <= self.maximum():
                continue
            x = QtWidgets.QStyle.sliderPositionFromValue(
                self.minimum(), self.maximum(), int(i), groove.width())
            x += groove.x()
            p.drawPolygon(QtGui.QPolygonF([
                QtCore.QPointF(x, y - 4.5), QtCore.QPointF(x + 3, y),
                QtCore.QPointF(x, y + 4.5), QtCore.QPointF(x - 3, y)]))
        p.end()


class BatchExport(jobs.Job):
    """批量导图片：不跑 ffmpeg，但共用同一条进度、同一个取消按钮。

    params_at(src) 允许逐帧改参数（去闪烁的曝光偏移）；返回 None 时用同一套。
    resume=True 时读 .lapse_progress.json 跳过已完成的帧，中途取消或崩了下次接着导。
    逐帧落盘那段和渲染队列走同一份 `renderq.render_frames`，两条路不会分叉。
    """

    def __init__(self, items, out_dir, p, quality=95, fmt="JPG", bit=8,
                 params_at=None, resume=True):
        super().__init__([])
        self.items, self.out_dir = list(items), out_dir
        self.p = copy.deepcopy(p)
        self.quality = quality
        self.fmt = fmt if fmt in exporter.EXPORT_FORMATS else "JPG"
        self.bit = 16 if int(bit) == 16 and self.fmt in ("PNG", "TIFF") else 8
        self.params_at = params_at
        self.resume = bool(resume)
        self.skipped = 0

    def run(self):
        res = renderq.render_frames(self, self.items, self.out_dir, self.p,
                                    self.params_at, self.quality, self.fmt,
                                    self.bit, self.resume)
        self.skipped = res.skipped
        if res.stopped:
            self.done.emit(False, f"已取消，剩余 {res.stopped} 张未导出")
            return
        self.done.emit(not res.bad, res.outs)


class ContactSheet(jobs.Job):
    """联系表：均匀抽几帧，每格左原图右调色后，一张 PNG 看整段前后差别。

    picks 是 (帧号, 路径) 的列表 —— 帧号用播放序列里的位置，标签和逐帧参数
    都按它取，体检排除掉几帧也不会错位。
    """

    def __init__(self, picks, dst, p, cell=380, cols=3, params_at=None):
        super().__init__([])
        self.picks, self.dst = list(picks), dst
        self.p = copy.deepcopy(p)
        self.cell, self.cols = int(cell), int(cols)
        self.params_at = params_at

    def run(self):
        pairs, labels, bad = [], [], []
        n = len(self.picks)
        for k, (i, src) in enumerate(self.picks):
            if self.stopping:
                self.done.emit(False, f"已取消，联系表未写出（看了 {k}/{n} 帧）")
                return
            im = exporter.load_small(src, self.cell * 2)
            if im is None:
                bad.append(i)
                self.progress.emit((k + 1) / max(n, 1), f"{k + 1}/{n}")
                continue
            p = self.params_at(src) if self.params_at else self.p
            try:
                after = np.clip(gc.apply(im, p), 0, 1)
            except Exception as e:
                # 单帧参数算崩不能拖死整张表：那一格退回原图，日志里留一句
                self.log.emit(f"第 {i + 1} 帧调色失败：{e}")
                after = im
            pairs.append((im, after))
            labels.append(f"#{i + 1:04d}")
            self.progress.emit((k + 1) / max(n, 1), f"{k + 1}/{n}")
        if bad:
            self.log.emit("读不出的帧：" + "、".join(f"#{i + 1}" for i in bad[:8]))
        if not pairs:
            self.done.emit(False, "抽中的帧一张都读不出，联系表没写成")
            return
        sheet = exporter.contact_sheet(pairs, labels, self.cell, self.cols)
        ok = exporter.save_image(self.dst, sheet.astype(np.float32) / 255.0,
                                 bit=8)
        if ok:
            self.done.emit(True, self.dst)
        else:
            try:                        # 写不进去就别留一个 0 字节的坑
                os.remove(self.dst)
            except OSError:
                pass
            self.done.emit(False, "写不出这个 PNG，换个有写权限的目录")


class Developer(Module):
    name = "调色台"
    docks = ()

    def __init__(self, shell):
        super().__init__(shell)
        self.store = params.Store(shell)
        self.grader = pipeline.Grader()        # 必须无父对象，见 pipeline.Grader
        self.grader.done.connect(self._on_render)
        self.grader.failed.connect(self._on_render_failed)
        self._render_failed = False
        self.work = None                       # float32 预览工作图
        self.proxy = None                      # 半尺寸草图，拖动时用
        self.src_path = None
        self.video = None                      # probe dict
        self.media_items = []
        self.sub = None
        self._clip = None
        self._tmp = None                       # 见 tmp_dir()
        self._ff = ffmod.find_ffmpeg()
        self._job_log = []
        self.player = playback.Player(self)    # 视频与序列的播放控制器
        self.player.frame.connect(self._on_play_frame)
        self.player.ended.connect(self._on_play_ended)
        self._play_items = []                  # 当前可播放的图片列表
        self._deflicker = None                 # 去闪烁测光与逐帧偏移
        self._deflicker_on = False             # 预览/导出是否套用
        self._flicker_sig = None               # 上次校验过的帧名单
        self._deflicker_dlg = None
        self._holy = None                      # 曝光台阶及逐帧补偿
        self._holy_on = False                  # 预览/导出是否套用
        self._holy_sig = None                  # 上次校验过的帧名单
        self._keyframes = []                   # [{index, params}]，序列关键帧
        self._keyframes_on = False              # 是否逐帧插值
        self._kf_files = []                    # 关键帧所属序列的帧名单
        self._kf_sig = None                    # 上次校验过的帧名单
        self._kf_current = None                # 当前显示/编辑的帧号
        self._kf_applying = False              # 正在套用插值，别回写关键帧
        self._curve_drag = None                # 正在拖亮度曲线时的那次编辑
        self._ref_path = None                  # 锁定的参考帧（跨帧对比用）
        self._ref_manual = False               # True = 用户指定，False = 跟上一关键帧
        self._ref_cache = None                 # 参考帧渲染结果（QImage）
        self._ref_sig = None                   # (路径, 参数签名)，变了才重渲
        self._compare_before_ref = False       # 进参考模式前的对比开关
        self._qa_excluded = set()               # 体检勾选要排除的帧（normcase）
        self._qa_sig = None                     # 上次体检对应的帧名单
        self._qa_dlg = None
        self._qa_kinds = {}                     # normcase 路径 -> 异常类型元组
        self._ratings = None                    # 当前序列文件夹的星级/色标表
        self._ratings_folder = None

    # ------------------------------------------------------------------ 装配
    def build(self):
        page = _Page(self)
        lay = QtWidgets.QVBoxLayout(page)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(0)
        self.subbar = self._build_subbar()
        self.subbar.setFixedHeight(38)
        lay.addWidget(self.subbar)

        # 没有素材时整块换成起始页：可点的入口比一行灰字有用
        self.start = startpage.StartPage()
        self.start.open_media.connect(self.shell.open_media_dialog)
        self.start.open_folder.connect(self.shell.open_folder_dialog)
        self.start.recent.connect(self._open_recent)
        self.start.tool.connect(self.shell.set_module)
        lay.addWidget(self.start, 1)

        self.view = imageview.ImageView()
        lay.addWidget(self.view, 1)
        self.transport = self._build_transport()
        lay.addWidget(self.transport)
        self.footer = self._build_viewer_footer()
        lay.addWidget(self.footer)
        self.view.zoom_changed.connect(self._sync_zoom)
        self._sync_zoom(self.view._zoom)

        # 图库是中央整窗网格；和显影底部的胶片条分开，滚轮/选中互不干扰
        self.grid_host = QtWidgets.QWidget()
        gv = QtWidgets.QVBoxLayout(self.grid_host)
        gv.setContentsMargins(0, 0, 0, 0)
        gv.setSpacing(0)
        gv.addWidget(self._build_filter_bar())
        self.grid_browser = filmstrip.Browser()
        self.grid_browser.picked.connect(self.select_index)
        self.grid_browser.activated.connect(self.open_in_develop)
        self.grid_browser.rated.connect(self._on_rated)
        gv.addWidget(self.grid_browser, 1)
        lay.addWidget(self.grid_host, 1)
        self.grid_host.hide()

        self._build_docks()
        self._build_menus()

        self.store.changed.connect(self._on_params_changed)
        self.store.history_changed.connect(self._update_history)
        self.view.mask_dragged.connect(self._on_mask_drag)
        self.view.mask_drag_state.connect(self._on_mask_drag_state)
        self.view.mask_component_selected.connect(self.masks.set_comp)
        self.view.mask_overlay_toggle.connect(self.masks.toggle_overlay)
        # 两级：拖动中每 50ms 出一张草图（900 长边），停手 260ms 补精修（2200）。
        # 草图用节流而不是防抖：防抖会每次改动都重置计时，连续拖动时一帧都不出。
        self.timer = QtCore.QTimer(self)
        self.timer.setSingleShot(True)
        self.timer.setInterval(50)
        self.timer.timeout.connect(lambda: self._render(False))
        self.idle = QtCore.QTimer(self)
        self.idle.setSingleShot(True)
        self.idle.setInterval(260)
        self.idle.timeout.connect(lambda: self._render(True))
        self._edit_key = None
        self._save_timer = QtCore.QTimer(self)
        self._save_timer.setSingleShot(True)
        self._save_timer.setInterval(700)
        self._save_timer.timeout.connect(self._flush_edits)
        self._kf_timer = QtCore.QTimer(self)
        self._kf_timer.setSingleShot(True)
        self._kf_timer.setInterval(500)
        self._kf_timer.timeout.connect(self._commit_keyframe)
        self.set_sub_mode("显影")
        self._update_history()
        self._update_keyframes_ui()
        self._sync_central()
        return page

    def _build_subbar(self):
        """子模式与历史操作：全部图标化，名字、用法、快捷键都在悬停提示里。"""
        bar = QtWidgets.QWidget()
        bar.setObjectName("SubBar")
        lay = QtWidgets.QHBoxLayout(bar)
        lay.setContentsMargins(8, 3, 8, 3)
        lay.setSpacing(4)
        self.sub_btns = {}
        for i, s in enumerate(SUBMODES):
            b = widgets.icon_button(SUB_ICONS[s],
                                    f"{s} · {SUB_HINT[s]}\nCtrl+Alt+{i + 1}",
                                    "dim", 18, 34, checkable=True,
                                    checked_role="accent_hi")
            b.clicked.connect(lambda _=False, x=s: self.set_sub_mode(x))
            lay.addWidget(b)
            self.sub_btns[s] = b
        lay.addStretch(1)
        self.btn_auto = widgets.icon_button(
            "自动", "自动 · 分析白平衡与影调（Ctrl+U）", "dim", 17, 32)
        self.btn_auto.clicked.connect(self.auto)
        self.btn_undo = widgets.icon_button(
            "undo", "撤销一步调色参数（Ctrl+Z）", "dim", 17, 32)
        self.btn_undo.clicked.connect(self.undo)
        self.btn_redo = widgets.icon_button(
            "redo", "重做一步调色参数（Ctrl+Shift+Z）", "dim", 17, 32)
        self.btn_redo.clicked.connect(self.redo)
        for x in (self.btn_auto, self.btn_undo, self.btn_redo):
            lay.addWidget(x)
        return bar

    def _build_filter_bar(self):
        """图库筛选条：按星级 / 色标 / 体检异常筛。只改视图，
        self.media_items 与体检、导出看到的全量都不动。"""
        bar = QtWidgets.QWidget()
        bar.setObjectName("SubBar")
        lay = QtWidgets.QHBoxLayout(bar)
        lay.setContentsMargins(8, 2, 8, 2)
        lay.setSpacing(5)
        self.fl_stars = QtWidgets.QSpinBox()
        self.fl_stars.setRange(0, ratings.MAX_STARS)
        self.fl_stars.setPrefix("≥ ")
        self.fl_stars.setSuffix(" 星")
        self.fl_stars.setToolTip("只显示至少这么多星的帧；0 星表示不看星级")
        self.fl_stars.valueChanged.connect(self._apply_filter)
        lay.addWidget(self.fl_stars)
        self.fl_label = QtWidgets.QComboBox()
        self.fl_label.addItem("色标：全部", None)
        for c in ratings.COLORS:
            self.fl_label.addItem("色标：" + ratings.color_name(c), c)
        self.fl_label.setToolTip("只显示带这个色标的帧")
        self.fl_label.currentIndexChanged.connect(self._apply_filter)
        lay.addWidget(self.fl_label)
        self.fl_qa = QtWidgets.QCheckBox("只看异常")
        self.fl_qa.setToolTip("先跑一次序列体检")
        self.fl_qa.setEnabled(False)
        self.fl_qa.toggled.connect(self._apply_filter)
        self.fl_qa.toggled.connect(self._sync_qa_filter_ui)
        lay.addWidget(self.fl_qa)
        self.fl_kind = QtWidgets.QComboBox()
        self.fl_kind.addItem("任意异常", None)
        for k in qa_mod.KINDS:
            self.fl_kind.addItem(qa_mod.label(k), k)
        self.fl_kind.setToolTip("只看某一类异常帧")
        self.fl_kind.setEnabled(False)
        self.fl_kind.currentIndexChanged.connect(self._apply_filter)
        lay.addWidget(self.fl_kind)
        self.fl_reset = QtWidgets.QPushButton("清除筛选")
        self.fl_reset.setObjectName("Chip")
        self.fl_reset.setToolTip("恢复显示全部帧")
        self.fl_reset.clicked.connect(self.clear_filter)
        lay.addWidget(self.fl_reset)
        lay.addStretch(1)
        self.fl_count = QtWidgets.QLabel("")
        self.fl_count.setObjectName("ViewerMeta")
        lay.addWidget(self.fl_count)
        bar.setFixedHeight(32)
        return bar

    # ------------------------------------------------------------------ 评级
    def _series_folder(self):
        """这些帧共用的那个文件夹；跨目录拼在一起时不落在任何一处。"""
        items = self.photo_items() or self.media_items
        if not items:
            return ""
        d = os.path.dirname(os.path.normpath(items[0]))
        if not d:
            return ""
        return d if all(os.path.dirname(os.path.normpath(p)) == d
                        for p in items) else ""

    def _sync_ratings(self):
        """序列换了文件夹就重读评级；星级存在素材旁边的 .lapse_ratings.json。"""
        folder = self._series_folder()
        if folder == self._ratings_folder and self._ratings is not None:
            return
        self._ratings_folder = folder
        self._ratings = ratings.Ratings(folder)
        self.browser.set_ratings(self._ratings)
        self.grid_browser.set_ratings(self._ratings)

    def _on_rated(self):
        self._update_filter_count()
        if self.shell.is_active(self):
            self.shell.set_page_hint(self.top_hint())
        # 报的是刚打过的那几帧，不是显影里正在看的那一帧。
        # 带筛选打分时列表会重建，选中可能整个没了，所以先认 just_rated。
        b = self.grid_browser if self.sub == "图库" else self.browser
        paths = b.just_rated() or b.selected_paths()
        if not paths or self._ratings is None:
            return
        stars, label = self._ratings.get(paths[0])
        idx = next((k for k, p in enumerate(self.media_items)
                    if p == paths[0]), None)
        text = (f"{len(paths)} 帧" if len(paths) > 1 or idx is None
                else "第 %d 帧" % (idx + 1))
        self.status(text + " · " + (f"{stars} 星" if stars else "未评星")
                    + (" · " + ratings.color_name(label) + "标"
                       if label != "none" else ""))

    # ------------------------------------------------------------------ 筛选
    def _apply_filter(self, *_args):
        spec = {}
        if self.fl_stars.value():
            spec["min_stars"] = self.fl_stars.value()
        label = self.fl_label.currentData()
        if label is not None:
            spec["label"] = label
        if self.fl_qa.isChecked():
            spec["qa_only"] = True
            kind = self.fl_kind.currentData()
            if kind is not None:
                spec["qa_kind"] = kind
        self.grid_browser.set_filter(spec)
        self._sync_qa_filter_ui()
        self._update_filter_count()
        if hasattr(self, "ex_filtered"):
            self.ex_filtered.setEnabled(bool(spec))
            if not spec:
                self.ex_filtered.setChecked(False)
            self.ex_filtered.setToolTip(
                ("当前筛出 %d 帧；勾上就只导这些"
                 % len(self.grid_browser.filtered_paths())) if spec else
                "图库页没有筛选条件；先筛再勾")
        if self.shell.is_active(self):
            self.shell.set_page_hint(self.top_hint())

    def clear_filter(self):
        for w in (self.fl_stars,):
            w.blockSignals(True)
            w.setValue(0)
            w.blockSignals(False)
        for w, row in ((self.fl_label, 0), (self.fl_kind, 0)):
            w.blockSignals(True)
            w.setCurrentIndex(row)
            w.blockSignals(False)
        self.fl_qa.blockSignals(True)
        self.fl_qa.setChecked(False)
        self.fl_qa.blockSignals(False)
        self._apply_filter()

    def _update_filter_count(self):
        shown = len(self.grid_browser.filtered_paths())
        total = len(self.grid_browser.paths)
        self.fl_count.setText(
            "" if not total else
            (f"{total} 帧" if shown == total else f"{shown} / {total} 帧"))

    def _sync_qa_filter_ui(self, *_args):
        if not hasattr(self, "fl_qa"):
            return
        has = bool(self._qa_kinds)
        self.fl_qa.setEnabled(has)
        self.fl_qa.setToolTip("只显示体检报出异常的帧" if has
                              else "先跑一次序列体检")
        if not has and self.fl_qa.isChecked():
            self.fl_qa.setChecked(False)
            return
        self.fl_kind.setEnabled(has and self.fl_qa.isChecked())

    def _build_transport(self):
        """播放条：视频和图片序列共用同一条时间轴，每一帧都过当前调色参数。"""
        bar = QtWidgets.QWidget()
        bar.setObjectName("TransportBar")
        bar.setFixedHeight(38)
        lay = QtWidgets.QHBoxLayout(bar)
        lay.setContentsMargins(8, 3, 10, 3)
        lay.setSpacing(3)

        self.btn_prev = widgets.icon_button("step-back", "上一帧（←）",
                                            "dim", 15, 28)
        self.btn_prev.clicked.connect(lambda: self.player.step(-1))
        self.btn_play = widgets.icon_button("play", "播放 / 暂停（空格）",
                                            "dim", 16, 30)
        self.btn_play.clicked.connect(self.toggle_play)
        self.btn_next = widgets.icon_button("step-forward", "下一帧（→）",
                                            "dim", 15, 28)
        self.btn_next.clicked.connect(lambda: self.player.step(1))
        self.btn_loop = widgets.icon_button("repeat", "循环播放", "dim",
                                            15, 28, checkable=True,
                                            checked_role="accent_hi")
        self.btn_loop.setChecked(True)
        self.btn_loop.toggled.connect(self._set_loop)
        for x in (self.btn_play, self.btn_prev, self.btn_next,
                  self.btn_loop):
            lay.addWidget(x)

        self.sl_time = TimeSlider()
        self.sl_time.setRange(0, 0)
        self.sl_time.setFocusPolicy(QtCore.Qt.FocusPolicy.NoFocus)
        self.sl_time.setToolTip("拖动时间轴定位；播放速率受解码速度限制")
        self.sl_time.sliderMoved.connect(lambda v: self.player.seek(v))
        lay.addWidget(self.sl_time, 1)

        self.lb_time = QtWidgets.QLabel("0/0")
        self.lb_time.setObjectName("TimeCode")
        self.lb_time.setFixedWidth(96)
        self.lb_time.setAlignment(QtCore.Qt.AlignmentFlag.AlignRight
                                  | QtCore.Qt.AlignmentFlag.AlignVCenter)
        lay.addWidget(self.lb_time)

        self.cb_speed = QtWidgets.QComboBox()
        for txt, mult in (("0.25×", 0.25), ("0.5×", 0.5), ("1×", 1.0),
                          ("2×", 2.0), ("4×", 4.0)):
            self.cb_speed.addItem(txt, mult)
        self.cb_speed.setCurrentIndex(2)
        self.cb_speed.setFixedWidth(62)
        self.cb_speed.setToolTip("播放倍速：4K 素材解码跟不上时调慢，先看运动")
        self.cb_speed.currentIndexChanged.connect(
            lambda i: self.player.set_speed(self.cb_speed.itemData(i)))
        lay.addWidget(self.cb_speed)
        bar.hide()
        return bar

    def _set_loop(self, on):
        self.player.loop = bool(on)

    def _sync_transport(self):
        n = self.player.count
        self.sl_time.setRange(0, max(0, n - 1))
        self.transport.setVisible(n > 1 and self.sub == "显影"
                                  and self.work is not None)
        self._show_time(self.player.index)
        self._update_keyframes_ui()

    def _show_time(self, i):
        """序列显示"第几张"，视频显示时间码。"""
        if self.video:
            fps = self.player.fps or 1.0
            self.lb_time.setText(f"{i / fps:.1f}s / "
                                 f"{(self.player.count - 1) / fps:.1f}s")
        else:
            self.lb_time.setText(f"{min(i + 1, self.player.count)}"
                                 f"/{self.player.count}")
        self.sl_time.blockSignals(True)
        self.sl_time.setValue(i)
        self.sl_time.blockSignals(False)

    def toggle_play(self, _checked=False):
        if self.player.count < 2:
            self.status("先打开一个视频或图片序列")
            return
        was_playing = self.player.playing
        self.player.toggle()
        self._sync_play_icon()
        if was_playing:
            self._sync_paused_frame()

    def _sync_play_icon(self):
        icons.repaint(self.btn_play,
                      "pause" if self.player.playing else "play",
                      "dim", 16)

    def _on_play_ended(self):
        """不循环时放到头就复位图标，省得按钮还显示着暂停。"""
        self._sync_play_icon()
        if not self.player.playing:
            self._sync_paused_frame()

    def _sync_paused_frame(self):
        """暂停后把参数面板对齐到当前帧：播放中只换画面，不动参数模型。"""
        i = self._current_play_index()
        if i is None:
            return
        self._kf_current = i
        if self._keyframes_on and self._keyframes:
            self._apply_keyframes_for(i)
            self._render()
        self._update_keyframes_ui()

    def _on_play_frame(self, idx, arr):
        """播放帧直接当工作图：走快速预览，跳过降噪与锐化这些慢活儿。"""
        self.work = arr.astype(np.float32) / 255.0
        self.proxy = pipeline.proxy_of(self.work)
        self.view.set_source(pipeline.to_qimage_u8(arr))
        if not self.video and self.browser.paths:
            self.browser.set_current(idx)
        self._show_time(idx)
        if not self.player.playing:
            self._kf_current = idx
            if self._keyframes_on and self._keyframes:
                self._apply_keyframes_for(idx)
            self._update_keyframes_ui()
        self.grader.submit(self.work, self._preview_params(idx), fast=True)

    def page_key(self, key):
        """播放条可见时接管空格与左右键：画布不吃的按键才轮得到这里。"""
        if not self.transport.isVisible():
            return False
        if key == QtCore.Qt.Key.Key_Space:
            self.toggle_play()
            return True
        if key in (QtCore.Qt.Key.Key_Left, QtCore.Qt.Key.Key_Right):
            self.player.step(-1 if key == QtCore.Qt.Key.Key_Left else 1)
            return True
        return False

    def _build_viewer_footer(self):
        """画布底部信息栏：文件名、尺寸/帧信息、缩放。

        和 Lightroom/Capture One 一样把图像信息压在画布下沿，
        顶部那条就只负责子模式，不再和素材信息抢位置。
        """
        bar = QtWidgets.QWidget()
        bar.setObjectName("ViewerFooter")
        bar.setFixedHeight(30)
        lay = QtWidgets.QHBoxLayout(bar)
        lay.setContentsMargins(10, 0, 10, 0)
        lay.setSpacing(10)
        self.lb_file = QtWidgets.QLabel("未打开")
        self.lb_file.setObjectName("ViewerFile")
        self.lb_file.setMinimumWidth(0)
        self.lb_file.setSizePolicy(QtWidgets.QSizePolicy.Policy.Ignored,
                                   QtWidgets.QSizePolicy.Policy.Preferred)
        self.lb_meta = QtWidgets.QLabel("")
        self.lb_meta.setObjectName("ViewerMeta")
        self.btn_zoom = QtWidgets.QToolButton()
        self.btn_zoom.setObjectName("ViewerZoom")
        self.btn_zoom.setPopupMode(
            QtWidgets.QToolButton.ToolButtonPopupMode.InstantPopup)
        self.btn_zoom.setCursor(QtCore.Qt.CursorShape.PointingHandCursor)
        self.btn_zoom.setToolTip("缩放倍率 · Ctrl+滚轮或双击画布")
        self.btn_zoom.setFixedWidth(72)
        self.zoom_menu = QtWidgets.QMenu(self.btn_zoom)
        act_fit = self.zoom_menu.addAction("适应窗口")
        act_fit.setShortcut("Ctrl+0")
        act_fit.triggered.connect(self.view.fit)
        self.zoom_menu.addSeparator()
        self.zoom_acts = {}
        for pct in (25, 50, 100, 200, 400, 800):
            a = self.zoom_menu.addAction(f"{pct}%")
            a.setCheckable(True)
            a.triggered.connect(lambda _=False, v=pct / 100.0:
                                self._zoom_to(v))
            self.zoom_acts[pct / 100.0] = a
        self.btn_zoom.setMenu(self.zoom_menu)
        lay.addWidget(self.lb_file)
        lay.addWidget(self.lb_meta)
        lay.addStretch(1)
        lay.addWidget(self.btn_zoom)
        return bar

    def _sync_zoom(self, z):
        self.btn_zoom.setText(f"{z * 100:.0f}%")
        self.btn_zoom.setToolTip(
            f"缩放 {z * 100:.0f}% · Ctrl+滚轮或双击画布切换适应/100%")
        for v, a in self.zoom_acts.items():
            a.setChecked(abs(z - v) < 0.005)

    def _zoom_to(self, z):
        """倍率菜单以画布中心为锚点，和 PS 的缩放级别一致。"""
        self.view.zoom_to(z)

    def _build_docks(self):
        sh = self.shell
        RIGHT = QtCore.Qt.DockWidgetArea.RightDockWidgetArea
        BOTTOM = QtCore.Qt.DockWidgetArea.BottomDockWidgetArea
        VERT = QtCore.Qt.Orientation.Vertical

        # 调整面板：影调 → 颜色 → 局部 → 曲线 → 预设，装在滚动区里
        self.tools = QtWidgets.QWidget()
        outer = QtWidgets.QVBoxLayout(self.tools)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(0)
        self.groups = []
        self.rows = []
        for title, spec in params.SLIDER_SPEC:
            if title == "黑白":
                outer.addWidget(self._make_mono_group(spec))
            else:
                # 只有基本校正默认展开，其余收起来：一屏滚不到头的右栏没人用
                outer.addWidget(self._make_group(title, spec,
                                                 collapsed=title != "基本"))
            # 色阶和基本校正一样是全局影调，紧挨着放；HSL 跟在色彩分级后面
            if title == "基本":
                outer.addWidget(self._make_levels_group())
            elif title == "色彩分级":
                outer.addWidget(self._make_hsl_group())
        outer.addWidget(self._make_mask_group())
        outer.addWidget(self._make_curve_group())
        outer.addWidget(self._make_par_group())
        outer.addWidget(self._make_preset_group())
        # 收尾留一条伸缩：分组都是固定高度，多余的空白全归这条，
        # 不会在折叠的分组之间撑出大空档
        outer.addStretch(1)

        scroll = QtWidgets.QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setWidget(self.tools)
        self.tools_scroll = scroll
        self.dock_tools = QtWidgets.QDockWidget("调整", sh)
        self.dock_tools.setWidget(scroll)
        self.dock_tools.setAllowedAreas(RIGHT | QtCore.Qt.DockWidgetArea.LeftDockWidgetArea)
        # 和其余三个模块的检查器同宽，切模块时右栏不会忽宽忽窄
        self.dock_tools.setMinimumWidth(374)
        sh.addDockWidget(RIGHT, self.dock_tools)

        self.scopes = scopes.Scopes()
        self.dock_scopes = QtWidgets.QDockWidget("示波器", sh)
        self.dock_scopes.setWidget(self.scopes)
        self.dock_scopes.setMinimumWidth(374)
        # 直方图是一条固定高度的读数条，不跟参数面板抢空间；
        # 上限 260 保证 12 个基本滑块能一次看到大半。
        self.dock_scopes.setMinimumHeight(190)
        self.dock_scopes.setMaximumHeight(260)
        sh.addDockWidget(RIGHT, self.dock_scopes)

        # 直方图在上、参数在下：和 Lightroom / Capture One 的右栏一致，
        # 也让 12 个基本滑块有足够高度，不用每调一项都滚半天。
        sh.splitDockWidget(self.dock_scopes, self.dock_tools, VERT)

        self.browser = filmstrip.Browser()
        self.browser.picked.connect(self.select_index)
        self.browser.activated.connect(self.open_in_develop)
        self.browser.rated.connect(self._on_rated)
        self.browser.curve.evBegin.connect(self._on_curve_begin)
        self.browser.curve.evEdit.connect(self._on_curve_edit)
        self.browser.curve.evEnd.connect(self._on_curve_end)
        self.dock_film = QtWidgets.QDockWidget("胶片", sh)
        self.dock_film.setWidget(self.browser)
        self.dock_film.setAllowedAreas(
            BOTTOM | QtCore.Qt.DockWidgetArea.TopDockWidgetArea)
        sh.addDockWidget(BOTTOM, self.dock_film)

        self.dock_export = QtWidgets.QDockWidget("导出", sh)
        self.dock_export.setWidget(self._build_export_panel())
        self.dock_export.setMinimumWidth(374)
        sh.addDockWidget(RIGHT, self.dock_export)

        self.docks = (self.dock_tools, self.dock_scopes,
                      self.dock_film, self.dock_export)

    def _make_group(self, title, spec, collapsed=False, extra=()):
        keys = [s[0] for s in spec] + list(extra)
        g = panels.Group(
            title, collapsed=collapsed,
            reset_fn=lambda ks=keys: self.store.reset_keys(ks))
        if title == "基本":
            # 自动是进入调色后最该先摸到的动作：放在滑块最上面，带文字
            row = g.vbox(6)
            b = QtWidgets.QPushButton("自动调整")
            b.setObjectName("Primary")
            b.setToolTip("分析白平衡与影调，生成可继续修改的起点（Ctrl+U）")
            icons.bind(b, "自动", "on_accent", 16)
            b.clicked.connect(self.auto)
            row.addWidget(b)
        for key, label, lo, hi, step, fmt, hint in spec:
            r = panels.SliderRow(self.store, label, key, lo, hi, step, fmt,
                                 hint=hint)
            self.rows.append(r)
            g.add(r)
        self.groups.append(g)
        return g

    def _make_mono_group(self, spec):
        """黑白：一条单色总量 + 8 个色相带的亮度混合（Lightroom 的 B&W Mixer）。"""
        g = self._make_group("黑白", spec, collapsed=True, extra=("mono_h",))
        for i, (name, hint) in enumerate(params.MONO_BANDS):
            r = panels.SliderRow(self.store, name, "mono_h", -100, 100, 1,
                                 "{:+.0f}", idx=i, name_w=52, hint=hint)
            self.rows.append(r)
            g.add(r)
        return g

    def _make_levels_group(self):
        self.levels = panels.LevelsCanvas(self.store)
        g = panels.Group(
            "色阶", reset_fn=lambda: self.store.reset_keys(params.LEVEL_KEYS))
        g.add(self.levels)
        self.groups.append(g)
        return g

    def _make_hsl_group(self):
        self.hsl = panels.HSLPanel(self.store)
        g = panels.Group(
            "HSL / 颜色混色", collapsed=True,
            reset_fn=lambda: self.store.reset_keys(["hsl_h", "hsl_s", "hsl_l"]))
        g.add(self.hsl)
        self.groups.append(g)
        return g

    def _make_curve_group(self):
        self.curve = panels.CurvePanel(self.store)
        g = panels.Group("色调曲线", collapsed=True)
        g.add(self.curve)
        self.groups.append(g)
        return g

    def _make_par_group(self):
        """参数曲线：和 LR 一样按亮度分区推，比点曲线更容易上手。"""
        g = panels.Group(
            "参数曲线", collapsed=True,
            reset_fn=lambda: self.store.reset_keys(params.PARAM_KEYS))
        for key, label, lo, hi, step, fmt, hint in params.PARAM_SLIDERS:
            r = panels.SliderRow(self.store, label, key, lo, hi, step, fmt,
                                 name_w=64, hint=hint)
            self.rows.append(r)
            g.add(r)
        self.groups.append(g)
        return g

    def _make_mask_group(self):
        """局部蒙版：几何在画面上拖，这里只管选中和增量数值。"""
        self.masks = panels.MaskPanel(self.store)
        g = panels.Group("局部蒙版",
                         reset_fn=lambda: self.store.mask_reset(self.masks.sel))
        g.add(self.masks)
        self.groups.append(g)
        self.masks.selected.connect(self.view.set_mask_sel)
        self.masks.component_selected.connect(self.view.set_mask_comp)
        self.masks.overlay_toggled.connect(self.view.set_mask_overlay)
        return g

    def _make_preset_group(self):
        g = panels.Group("预设")
        self.groups.append(g)
        self.preset_btns = {}
        row = None
        for i, name in enumerate(gc.PRESETS):
            if i % 3 == 0:
                row = QtWidgets.QHBoxLayout()
                row.setSpacing(4)
                g.form.addLayout(row)
            b = QtWidgets.QPushButton(name)
            b.setObjectName("Chip")
            b.setCheckable(True)
            b.clicked.connect(lambda _=False, n=name: self.store.load_preset(n))
            row.addWidget(b)
            self.preset_btns[name] = b
        bar = QtWidgets.QHBoxLayout()
        bar.setSpacing(4)
        b1 = QtWidgets.QPushButton("全部复位")
        b1.setObjectName("Chip")
        b1.setToolTip("把所有参数恢复到默认值（Ctrl+R）")
        b1.clicked.connect(self.store.reset_all)
        b2 = QtWidgets.QPushButton("复制设置")
        b2.setObjectName("Chip")
        b2.clicked.connect(self._copy_settings)
        b3 = QtWidgets.QPushButton("粘贴设置")
        b3.setObjectName("Chip")
        b3.clicked.connect(self._paste_settings)
        for x in (b1, b2, b3):
            bar.addWidget(x)
        g.form.addLayout(bar)
        return g

    def _sync_preset_btns(self):
        """预设只在参数与它完全一致时高亮：手动改过一项就全部取消。"""
        cur = self.store.p
        for name, b in self.preset_btns.items():
            p = gc.preset(name)
            b.setChecked(all(cur.get(k) == v for k, v in p.items()))

    def _build_export_panel(self):
        w = QtWidgets.QWidget()
        lay = QtWidgets.QVBoxLayout(w)
        lay.setContentsMargins(10, 10, 10, 10)
        lay.setSpacing(7)

        lay.addWidget(self._h("静态图片"))
        row = QtWidgets.QHBoxLayout()
        self.ex_fmt = QtWidgets.QComboBox()
        self.ex_fmt.addItems(["JPG", "PNG", "TIFF"])
        self.ex_bit = QtWidgets.QComboBox()
        self.ex_bit.addItem("8 位", 8)
        self.ex_bit.addItem("16 位", 16)
        self.ex_bit.setCurrentIndex(1)
        self.ex_bit.setToolTip("16 位只在 PNG/TIFF 里保留，适合继续后期；JPG 固定 8 位")
        self.ex_fmt.currentTextChanged.connect(self._sync_bit_depth)
        self.ex_q = QtWidgets.QSpinBox()
        self.ex_q.setRange(60, 100)
        self.ex_q.setValue(95)
        self.ex_q.setToolTip("只影响 JPG；PNG/TIFF 不用压缩参数")
        row.addWidget(self.ex_fmt, 1)
        row.addWidget(self.ex_bit)
        row.addWidget(QtWidgets.QLabel("质量"))
        row.addWidget(self.ex_q)
        lay.addLayout(row)
        self._sync_bit_depth(self.ex_fmt.currentText())
        self.btn_img = QtWidgets.QPushButton("导出当前…")
        self.btn_img.clicked.connect(self.export_image)
        self.btn_batch = QtWidgets.QPushButton("导出整个文件夹…")
        self.btn_batch.clicked.connect(self.export_batch)
        self.ex_filtered = QtWidgets.QCheckBox("只导出筛选出来的帧")
        self.ex_filtered.setToolTip(
            "按图库页顶部的筛选条件来；没筛或没勾就导出整个文件夹。"
            "体检排除的帧仍然会跳过")
        lay.addWidget(self.btn_img)
        lay.addWidget(self.ex_filtered)
        lay.addWidget(self.btn_batch)

        lay.addWidget(self._h("前后对照联系表"))
        row = QtWidgets.QHBoxLayout()
        row.addWidget(QtWidgets.QLabel("抽取"))
        self.ex_sheet_n = QtWidgets.QComboBox()
        for k in (6, 12, 24, 48):
            self.ex_sheet_n.addItem(f"{k} 帧", k)
        self.ex_sheet_n.setCurrentIndex(1)
        row.addWidget(self.ex_sheet_n, 1)
        row.addWidget(QtWidgets.QLabel("排布"))
        self.ex_sheet_cols = QtWidgets.QComboBox()
        for k in (2, 3, 4):
            self.ex_sheet_cols.addItem(f"{k} 列", k)
        self.ex_sheet_cols.setCurrentIndex(1)      # 12 帧 × 3 列 ≈ 4 行，不至于竖成一柱
        row.addWidget(self.ex_sheet_cols, 1)
        lay.addLayout(row)
        self.btn_sheet = QtWidgets.QPushButton("导出联系表（调色前后）…")
        self.btn_sheet.clicked.connect(self.export_sheet)
        hints.attach(self.btn_sheet, "导出/联系表")
        self.ex_sheet_n.setToolTip(hints.text("导出/联系表"))
        lay.addWidget(self.btn_sheet)

        lay.addWidget(self._h("序列关键帧"))
        row = QtWidgets.QHBoxLayout()
        self.cb_keyframes = QtWidgets.QCheckBox("逐帧插值")
        self.cb_keyframes.setToolTip(
            "在关键帧之间逐参数插值；改参数会自动记录到当前帧（需要图片序列）")
        self.cb_keyframes.toggled.connect(self.set_keyframes_on)
        self.btn_kf_record = QtWidgets.QPushButton("记录")
        self.btn_kf_record.setObjectName("Chip")
        self.btn_kf_record.setToolTip("把当前帧的参数记录为关键帧（K）")
        self.btn_kf_record.clicked.connect(self.record_keyframe)
        self.btn_kf_remove = QtWidgets.QPushButton("删除")
        self.btn_kf_remove.setObjectName("Chip")
        self.btn_kf_remove.setToolTip("删除当前帧的关键帧（Shift+K）")
        self.btn_kf_remove.clicked.connect(self.remove_keyframe)
        self.btn_kf_clear = QtWidgets.QPushButton("清空")
        self.btn_kf_clear.setObjectName("Chip")
        self.btn_kf_clear.setToolTip("删掉整个序列的关键帧，恢复一套参数")
        self.btn_kf_clear.clicked.connect(self.clear_keyframes)
        row.addWidget(self.cb_keyframes, 1)
        for b in (self.btn_kf_record, self.btn_kf_remove, self.btn_kf_clear):
            row.addWidget(b)
        lay.addLayout(row)
        self.lb_keyframes = self._note("需要至少两张图片的序列")
        lay.addWidget(self.lb_keyframes)

        lay.addWidget(self._h("延时去闪烁"))
        row = QtWidgets.QHBoxLayout()
        self.btn_flicker = QtWidgets.QPushButton("分析序列…")
        self.btn_flicker.setToolTip("逐帧测光，给闪烁帧补曝光；只对图片序列生效")
        self.btn_flicker.clicked.connect(self.open_deflicker)
        self.cb_flicker = QtWidgets.QCheckBox("应用校正")
        self.cb_flicker.setToolTip("预览与批量导出套用逐帧曝光偏移；视频导出暂不套用")
        self.cb_flicker.setEnabled(False)
        self.cb_flicker.toggled.connect(self._on_flicker_toggle)
        row.addWidget(self.btn_flicker, 1)
        row.addWidget(self.cb_flicker)
        lay.addLayout(row)
        self.lb_flicker = self._note("未分析")
        lay.addWidget(self.lb_flicker)

        lay.addWidget(self._h("曝光台阶"))
        row = QtWidgets.QHBoxLayout()
        self.btn_holy = QtWidgets.QPushButton("检测台阶")
        self.btn_holy.setToolTip(
            "读 EXIF 找相机改 ISO/快门/光圈的帧；读不到就用亮度曲线推断")
        self.btn_holy.clicked.connect(self.detect_holy)
        self.btn_holy.setEnabled(False)
        self.cb_holy = QtWidgets.QCheckBox("应用补偿")
        self.cb_holy.setToolTip("把台阶摊成斜坡，预览与批量导出都套用")
        self.cb_holy.setEnabled(False)
        self.cb_holy.toggled.connect(self._on_holy_toggle)
        row.addWidget(self.btn_holy, 1)
        row.addWidget(self.cb_holy)
        lay.addLayout(row)
        row = QtWidgets.QHBoxLayout()
        row.addWidget(QtWidgets.QLabel("过渡"))
        self.sp_holy_ramp = QtWidgets.QSpinBox()
        self.sp_holy_ramp.setRange(2, edits.MAX_HOLY_RAMP)
        self.sp_holy_ramp.setValue(holly.RAMP)
        self.sp_holy_ramp.setSuffix(" 帧")
        self.sp_holy_ramp.setToolTip("台阶摊开成多少帧的斜坡；太短仍会跳，太长会拖影")
        self.sp_holy_ramp.setEnabled(False)
        self.sp_holy_ramp.valueChanged.connect(self._on_holy_ramp)
        row.addWidget(self.sp_holy_ramp)
        row.addStretch(1)
        self.btn_holy_clear = QtWidgets.QPushButton("清除")
        self.btn_holy_clear.setObjectName("Chip")
        self.btn_holy_clear.setToolTip("删掉检测结果与补偿")
        self.btn_holy_clear.setEnabled(False)
        self.btn_holy_clear.clicked.connect(lambda: self.clear_holy())
        row.addWidget(self.btn_holy_clear)
        lay.addLayout(row)
        self.lb_holy = self._note("需要至少两张图片的序列")
        lay.addWidget(self.lb_holy)

        lay.addWidget(self._h("序列体检"))
        row = QtWidgets.QHBoxLayout()
        self.btn_qa = QtWidgets.QPushButton("体检…")
        self.btn_qa.setToolTip(
            "逐帧找坏帧、黑帧、虚焦、曝光离群、重复帧和缺口；"
            "勾选的帧在批量导出时跳过")
        self.btn_qa.clicked.connect(self.open_qa)
        self.btn_qa.setEnabled(False)
        row.addWidget(self.btn_qa, 1)
        self.btn_qa_clear = QtWidgets.QPushButton("清除排除")
        self.btn_qa_clear.setObjectName("Chip")
        self.btn_qa_clear.setToolTip("取消体检勾选的排除，导出恢复全帧")
        self.btn_qa_clear.clicked.connect(self.clear_qa)
        self.btn_qa_clear.setEnabled(False)
        row.addWidget(self.btn_qa_clear)
        lay.addLayout(row)
        self.lb_qa = self._note("未体检")
        lay.addWidget(self.lb_qa)

        lay.addWidget(self._h("视频"))
        row = QtWidgets.QHBoxLayout()
        self.ex_crf = QtWidgets.QSpinBox()
        self.ex_crf.setRange(0, 40)
        self.ex_crf.setValue(18)
        self.ex_crf.setToolTip("CRF 越小越清晰、文件越大；4K 一般 17~20")
        self.ex_preset = QtWidgets.QComboBox()
        for label, value in ffmod.PRESET_LABELS:
            self.ex_preset.addItem(label, value)
        self.ex_preset.setCurrentIndex(self.ex_preset.findData("fast"))
        self.ex_preset.setToolTip("编码速度：越靠前越快、体积越大")
        row.addWidget(QtWidgets.QLabel("CRF"))
        row.addWidget(self.ex_crf)
        row.addWidget(self.ex_preset, 1)
        lay.addLayout(row)
        self.btn_vid = QtWidgets.QPushButton("导出调色视频…")
        self.btn_vid.clicked.connect(self.export_video)
        lay.addWidget(self.btn_vid)
        lay.addWidget(self._note(
            "颜色 / 色阶 / 黑白 / 局部蒙版都会带上；镜头校正（畸变、暗角补偿、"
            "去紫边）是空间运算，LUT 表达不了，只进静态图。"
            "清晰度与纹理用等效滤镜近似；颗粒固定不闪但粗细固定"
            "（大小 / 密度只影响预览与静态图）；降噪只做空间域。"
            "亮度范围蒙版在视频里按首帧亮度计算，不随画面逐帧变化。"))

        lay.addWidget(self._h("渲染队列（多序列）"))
        self.lb_queue = QtWidgets.QListWidget()
        self.lb_queue.setObjectName("QueueList")
        self.lb_queue.setMaximumHeight(122)
        self.lb_queue.setSelectionMode(
            QtWidgets.QAbstractItemView.SelectionMode.ExtendedSelection)
        hints.attach(self.lb_queue, "导出/渲染队列")
        lay.addWidget(self.lb_queue)
        row = QtWidgets.QHBoxLayout()
        self.btn_q_add = QtWidgets.QPushButton("加入当前序列")
        self.btn_q_del = QtWidgets.QPushButton("移出")
        self.btn_q_del.setObjectName("Chip")
        self.btn_q_clear = QtWidgets.QPushButton("清空")
        self.btn_q_clear.setObjectName("Chip")
        self.btn_q_add.clicked.connect(self.queue_add)
        self.btn_q_del.clicked.connect(self.queue_remove)
        self.btn_q_clear.clicked.connect(self.queue_clear)
        hints.attach(self.btn_q_add, "导出/队列加入")
        row.addWidget(self.btn_q_add, 1)
        row.addWidget(self.btn_q_del)
        row.addWidget(self.btn_q_clear)
        lay.addLayout(row)
        row = QtWidgets.QHBoxLayout()
        row.addWidget(QtWidgets.QLabel("帧率"))
        self.q_fps = QtWidgets.QComboBox()
        self.q_fps.addItems(FPS_CHOICES)
        self.q_fps.setEditable(True)
        self.q_fps.setCurrentText("120")
        hints.attach(self.q_fps, "合成/帧率")
        row.addWidget(self.q_fps, 1)
        row.addWidget(QtWidgets.QLabel("长边"))
        self.q_side = QtWidgets.QComboBox()
        self.q_side.addItems(SIDE_CHOICES)
        self.q_side.setEditable(True)
        self.q_side.setCurrentText(SIDE_CHOICES[0])
        hints.attach(self.q_side, "合成/长边")
        row.addWidget(self.q_side, 1)
        lay.addLayout(row)
        row = QtWidgets.QHBoxLayout()
        row.addWidget(QtWidgets.QLabel("比例"))
        self.q_aspect = QtWidgets.QComboBox()
        self.q_aspect.addItems([t for t, _ in ASPECT_CHOICES])
        hints.attach(self.q_aspect, "合成/比例")
        row.addWidget(self.q_aspect, 1)
        row.addWidget(QtWidgets.QLabel("编码"))
        self.q_codec = QtWidgets.QComboBox()
        self.q_codec.addItems([t for t, _ in CODEC_CHOICES])
        hints.attach(self.q_codec, "合成/编码器")
        row.addWidget(self.q_codec, 1)
        lay.addLayout(row)
        self.q_keep = QtWidgets.QCheckBox("保留中间调色帧")
        hints.attach(self.q_keep, "导出/队列留帧")
        lay.addWidget(self.q_keep)
        self.btn_queue = QtWidgets.QPushButton("渲染队列全部…")
        self.btn_queue.setObjectName("Primary")
        self.btn_queue.clicked.connect(self.render_queue)
        hints.attach(self.btn_queue, "导出/渲染队列")
        lay.addWidget(self.btn_queue)
        lay.addWidget(self._note(
            "每一段用的是它自己文件夹里存好的调色（参数、逐帧插值、去闪烁、"
            "曝光台阶），不是屏幕上现在这套。取消或崩溃后再点一次，会从断掉"
            "的那一帧接着渲；改过某一段的调色，它的中间帧自动作废重渲。"))
        self._queue = renderq.load()
        self._queue_refresh()

        lay.addWidget(self._h("3D LUT"))
        row = QtWidgets.QHBoxLayout()
        self.ex_lut = QtWidgets.QComboBox()
        self.ex_lut.addItems(["33", "65"])
        self.ex_lut.setCurrentText("65")
        self.ex_lut.setToolTip("LUT 边长：65 精度高但文件大，33 通用")
        row.addWidget(QtWidgets.QLabel("尺寸"))
        row.addWidget(self.ex_lut, 1)
        lay.addLayout(row)
        b = QtWidgets.QPushButton("导出 .cube…")
        b.clicked.connect(self.export_cube)
        lay.addWidget(b)
        lay.addWidget(self._note(
            ".cube 只装颜色类运算（含色阶、黑白、色彩分级）；锐化、降噪、晕影、"
            "颗粒、蒙版和镜头校正都是空间效果，不进 LUT。"))
        lay.addStretch(1)
        # 导出面板现在有九节，dock 里必须能滚，否则最后几节永远看不到
        scroll = QtWidgets.QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QtWidgets.QFrame.Shape.NoFrame)
        scroll.setWidget(w)
        return scroll

    def _sync_bit_depth(self, fmt):
        jpg = str(fmt).upper() == "JPG"
        self.ex_bit.setEnabled(not jpg)
        if jpg:
            self.ex_bit.setCurrentIndex(0)

    def _export_bit(self):
        fmt = self.ex_fmt.currentText().upper()
        return 8 if fmt == "JPG" else int(self.ex_bit.currentData())

    @staticmethod
    def _h(text):
        lb = QtWidgets.QLabel(text)
        lb.setObjectName("DockTitle")
        return lb

    @staticmethod
    def _note(text):
        lb = QtWidgets.QLabel(text)
        lb.setObjectName("ViewerMeta")
        lb.setWordWrap(True)
        return lb

    def _build_menus(self):
        sh = self.shell
        f = sh.menu("文件")
        for text, slot in (("导出图片…", self.export_image),
                           ("批量导出…", self.export_batch),
                           ("导出视频…", self.export_video),
                           ("导出 3D LUT…", self.export_cube)):
            f.addAction(self._act(text, slot))
        f.addSeparator()
        f.addAction(self._act("导入 XMP 预设…", self.import_xmp))
        f.addAction(self._act("导出 XMP 预设…", self.export_xmp))

        t = sh.menu("调整")
        t.addAction(self._act("自动校正", self.auto, "Ctrl+U"))
        self.act_flicker = self._act("序列去闪烁…", self.open_deflicker)
        self.act_flicker.setEnabled(False)
        t.addAction(self.act_flicker)
        self.act_holy = self._act("检测曝光台阶", self.detect_holy)
        self.act_holy.setEnabled(False)
        t.addAction(self.act_holy)
        t.addSeparator()
        self.act_kf_on = self._act("序列关键帧插值", self.set_keyframes_on)
        self.act_kf_on.setCheckable(True)
        self.act_kf_on.setEnabled(False)
        t.addAction(self.act_kf_on)
        self.act_kf_record = self._act("记录关键帧", self.record_keyframe, "K")
        self.act_kf_record.setEnabled(False)
        t.addAction(self.act_kf_record)
        self.act_kf_remove = self._act("删除当前关键帧", self.remove_keyframe,
                                       "Shift+K")
        self.act_kf_remove.setEnabled(False)
        t.addAction(self.act_kf_remove)
        for text, slot, sc in (
                ("撤销", self.undo, "Ctrl+Z"),
                ("重做", self.redo, "Ctrl+Shift+Z"),
                ("全部复位", self.store.reset_all, "Ctrl+R")):
            t.addAction(self._act(text, slot, sc))

        v = sh.menu("视图")
        self.act_compare = self._act("前后分割对比", lambda: self.toggle_compare())
        self.act_compare.setShortcut("Ctrl+\\")
        self.act_compare.setCheckable(True)
        self.act_compare.toggled.connect(self.view.set_compare)
        v.addAction(self.act_compare)
        self.act_ref = QtGui.QAction("参考帧对比", self.shell)
        self.act_ref.setShortcut("Ctrl+Shift+R")
        self.act_ref.setCheckable(True)
        self.act_ref.setEnabled(False)
        self.act_ref.toggled.connect(self.toggle_reference)
        v.addAction(self.act_ref)
        self.act_ref_lock = self._act("锁定当前帧为参考", self.lock_reference,
                                      "Ctrl+Alt+R")
        self.act_ref_lock.setEnabled(False)
        v.addAction(self.act_ref_lock)
        a = self._act("清除参考帧", self.clear_reference)
        a.setEnabled(False)
        self.act_ref_clear = a
        v.addAction(a)
        for text, slot, sc in (("适应窗口", self.view.fit, "Ctrl+0"),
                               ("100%", lambda: self.view.zoom_to(1.0), "Ctrl+1"),
                               ("200%", lambda: self.view.zoom_to(2.0), "Ctrl+2")):
            v.addAction(self._act(text, slot, sc))
        v.addSeparator()
        for name, dock in (("调整面板", self.dock_tools),
                           ("示波器", self.dock_scopes),
                           ("胶片", self.dock_film),
                           ("导出", self.dock_export)):
            a = dock.toggleViewAction()
            a.setText(name)
            v.addAction(a)
        v.addSeparator()
        # Ctrl+1/2 已被缩放占用，子模式用 Ctrl+Alt+序号
        for i, s in enumerate(SUBMODES):
            v.addAction(self._act(f"转到 {s}",
                                  lambda _=False, x=s: self.set_sub_mode(x),
                                  f"Ctrl+Alt+{i + 1}"))

    def _act(self, text, slot, sc=None):
        a = QtGui.QAction(text, self.shell)
        if sc:
            a.setShortcut(sc)
        a.triggered.connect(slot)
        return a

    # ------------------------------------------------------------------ 子模式
    def visible_docks(self):
        return tuple(getattr(self, n) for n in SUB_DOCKS.get(self.sub, ()))

    def enter(self):
        self.set_sub_mode(self.sub or "显影")
        self._sync_play_icon()
        self._render()
        # 首次进入时窗口还没显示，resizeDocks 量不到高度；等布局跑完再定胶片条
        QtCore.QTimer.singleShot(0, self._layout_film)

    def leave(self):
        """切到别的模块就停下：播放不该在后台继续烧解码 CPU。"""
        if self.player.playing:
            self.player.pause()
            self._sync_play_icon()

    def hint(self):
        # 状态栏只放状态不放教程：快捷键说明在子模式按钮的悬停提示里
        if self.sub == "图库" and self.media_items:
            return f"{len(self.media_items)} 张"
        return ""

    def top_hint(self):
        if self.sub == "图库" and self.media_items:
            shown = len(self.grid_browser.filtered_paths())
            total = len(self.media_items)
            return f"{total} 张" if shown >= total else f"{shown} / {total} 张"
        return ""

    def set_sub_mode(self, s):
        self.sub = s
        for k, b in self.sub_btns.items():
            b.setChecked(k == s)
        # 自动/撤销/重做只对调色参数有意义，图库和导出里不占位置
        for b in (self.btn_auto, self.btn_undo, self.btn_redo):
            b.setVisible(s == "显影")
        grid = s == "图库"
        # 图库切换中央页：网格吃满整个内容区，预览与底栏让位
        self._sync_central()
        if grid:
            self.grid_browser.set_mode("grid")
            self.grid_browser.set_items(self.media_items, self.browser.index)
        else:
            self.browser.set_mode("strip")
        # 胶片条要能装下一个完整单元格（含亮度曲线），否则星级会被视口裁掉
        self.dock_film.setMinimumHeight(124 if grid else self._film_dock_h())
        self.shell.refresh_docks()
        self._layout_film()
        self._sync_transport()
        self.status(self.hint())
        if self.shell.is_active(self):
            self.shell.set_page_hint(self.top_hint())

    def _sync_central(self):
        """起始页 / 画布 / 图库三者只留一个，避免空窗口或双份空白。"""
        empty = self.work is None
        grid = self.sub == "图库"
        self.start.setVisible(empty)
        if empty:
            self.start.set_recent(prefs.recent_projects())
        self.view.setVisible(not empty and not grid)
        self.footer.setVisible(not empty and not grid)
        self.grid_host.setVisible(not empty and grid)
        self._sync_transport()

    def refresh_start(self):
        self.start.set_recent(prefs.recent_projects())

    def _open_recent(self, path):
        if not os.path.exists(path):
            prefs.remove_recent(path)
            self.start.set_recent(prefs.recent_projects())
            self.status("路径已不存在：" + path)
            return
        name = self.shell.open_path(path)
        if name and self.shell.module(name):
            self.shell.set_module(name)

    def _film_dock_h(self):
        """胶片条停靠窗该多高：控件自己算的下限 + 标题栏和边框那 26px。"""
        return self.browser.minimumHeight() + 26

    def _layout_film(self):
        """显影只留一条胶片；图库没有胶片条，网格吃满中央页。"""
        sh = self.shell
        if not sh.isVisible() or self.sub == "图库":
            return
        # 停靠高度按胶片条自己的下限来；写死 150 会把星级压到视口外
        sh.resizeDocks([self.dock_film], [self._film_dock_h()],
                       QtCore.Qt.Orientation.Vertical)

    # ------------------------------------------------------------------ 素材
    def on_material(self, kind, path):
        """流水线：合成/裁剪写完就切到这里预览。"""
        if not path:
            return
        if kind == "video":
            self.load_video(path)
        elif kind == "image":
            # 批量打开时 Material 只指向第一张，图库已经装好了就别重载成单张
            if path in self.media_items:
                # 但画面还停在上一条视频上时必须切回来，不然素材说是图片、
                # 画布演的还是视频
                if self.video is not None:
                    self.select_index(self.media_items.index(path))
            else:
                self.set_media_list([path])
        elif kind == "series":
            self.open_folder(path)

    def open_media_list(self, paths):
        """外壳一次打开多张图片/RAW 时，整批装进图库。"""
        items = [p for p in paths if os.path.isfile(p)]
        if not items:
            return False
        self.set_media_list(items)
        return True

    def open_folder(self, d):
        items = sorted(os.path.join(d, n) for n in os.listdir(d)
                       if os.path.splitext(n)[1].lower() in IMG_EXT + VID_EXT)
        if items:
            self.set_media_list(items)
        else:
            self.status("文件夹里没有可打开的媒体：" + d)

    def set_media_list(self, items, current=0):
        self.media_items = list(items)
        if self._ref_path and self._ref_path not in self.media_items:
            self._ref_path = None
            self._ref_manual = False
            self._ref_cache = self._ref_sig = None
            if self.act_ref.isChecked():
                self.act_ref.setChecked(False)
            self.view.set_reference(None)
        self._sync_ratings()
        self.browser.set_items(self.media_items, current)
        if self.sub == "图库":
            self.grid_browser.set_items(self.media_items, current)
            self._update_filter_count()
            self.status(self.hint())
            if self.shell.is_active(self):
                self.shell.set_page_hint(self.top_hint())
        self.select_index(current)
        self._sync_ref_actions()

    def select_index(self, i):
        if not (0 <= i < len(self.media_items)):
            return
        self.browser.index = i
        self.browser.set_current(i)
        if self.grid_browser.paths:
            self.grid_browser.set_current(i)
        self.load_media(self.media_items[i])

    def select_path(self, path):
        """把当前列表停在指定那一帧（体检表双击跳帧用）。"""
        want = os.path.normcase(os.path.normpath(path))
        for i, p in enumerate(self.media_items):
            if os.path.normcase(os.path.normpath(p)) == want:
                self.select_index(i)
                return True
        return False

    def open_in_develop(self, i):
        """图库双击：载入这张并切到显影。"""
        self.select_index(i)
        self.set_sub_mode("显影")

    def load_media(self, path):
        ext = os.path.splitext(path)[1].lower()
        if ext in VID_EXT:
            self.load_video(path)
        else:
            self.load_image(path)

    def load_image(self, path):
        im = pipeline.load_u8(path)
        if im is None:
            self.status("无法读取：" + path)
            return
        self.src_path = path
        self.video = None
        small = pipeline.fit_long(im)
        self.work = small.astype(np.float32) / 255.0
        self.proxy = pipeline.proxy_of(self.work)
        self.view.set_source(pipeline.to_qimage_u8(small))
        self._sync_central()
        h, w = im.shape[:2]
        self.lb_file.setText(os.path.basename(path))
        self.lb_file.setToolTip(path)
        self.lb_meta.setText(" · ".join(
            x for x in (self._index_text(), f"{w}×{h}") if x))
        self._install_sequence(path)
        self._switch_edits(path)
        self._sync_ref_actions()
        self._refresh_reference()
        self.status(f"预览工作图 {self.work.shape[1]}×{self.work.shape[0]}"
                    f"（原图 {w}×{h}）")
        self._render()

    def _install_sequence(self, path):
        """图片列表一装好就能按序播放：换列表才重建播放器，换张只挪指针。"""
        items = [p for p in self.media_items
                 if os.path.splitext(p)[1].lower() in IMG_EXT]
        if len(items) < 2:
            self._play_items = []
            self.player.set_source(None, 0, prime=False)
            self._sync_transport()
            return
        i = items.index(path) if path in items else 0
        if items != self._play_items:
            self._play_items = items
            self.player.set_source(playback.ListSource(items), i, prime=False)
        else:
            self.player.park(i)
        self._sync_transport()

    def load_video(self, path):
        if not self._ff:
            self.status("未找到 ffmpeg")
            return
        # 导入时后台已经做好"探测 + 一张预览帧"，这里只取现成的；
        # 命令行或流水线直接调用时才在这儿现算一次
        bundle = ffmod.import_bundle(self._ff, path)
        info = bundle["info"] if bundle else None
        if not info:
            self.status("不是可读取的视频：" + path)
            return
        self.video = info
        self.src_path = path
        self._play_items = []
        self._ref_path = None
        self._ref_manual = False
        self._ref_cache = self._ref_sig = None
        if self.act_ref.isChecked():
            self.act_ref.setChecked(False)
        self.view.set_reference(None)
        self.lb_file.setText(os.path.basename(path))
        self.lb_file.setToolTip(path)
        self.lb_meta.setText(" · ".join(
            x for x in (self._index_text(), ffmod.disp_text(info),
                        f"{info['fps']:.2f}fps",
                        f"{info['dur']:.1f}s") if x))
        # 先挂播放条：抽帧失败也不该整个预览跟着没
        t = info["dur"] * 0.25 if info["dur"] else 0
        src = playback.VideoSource(self._ff, path, info)
        start = int(t * src.fps)
        self.player.set_source(src, start, prime=False)
        self._sync_transport()
        self._switch_edits(path)
        self._sync_ref_actions()
        im = pipeline.decode_u8(bundle["png"])
        if im is None:
            # 没有静帧就当第一帧是工作图：起线程解一帧出来
            self.player.seek(start)
            self.status("抽帧失败：直接从视频解预览帧")
            return
        small = pipeline.fit_long(im)
        self.work = small.astype(np.float32) / 255.0
        self.proxy = pipeline.proxy_of(self.work)
        self.view.set_source(pipeline.to_qimage_u8(small))
        self._sync_central()
        self.status("视频：空格播放整片，调色参数逐帧实时套用")
        self._render()

    def _index_text(self):
        n = len(self.media_items)
        return f"{self.browser.index + 1}/{n}" if n > 1 else ""

    # ------------------------------------------------------------ 参数自动落盘
    def _edit_key_for(self, path):
        """一个序列共用一套调色（延时片段的常态），单图/视频各存各的。"""
        path = os.path.normpath(path)
        if self.video is not None:
            return "video:" + path
        items = self._play_items or []
        dirs = {os.path.dirname(os.path.normpath(p)) for p in items}
        if len(items) > 1 and len(dirs) == 1:
            return "series:" + dirs.pop()
        return "image:" + path

    def _switch_edits(self, path):
        key = self._edit_key_for(path)
        changed = key != self._edit_key
        if changed:
            self._flush_edits()
            self._edit_key = key
            p = edits.load(key) if edits.enabled() else None
            self.store.load(p if p is not None else gc.defaults())
        if changed or self._keyframe_signature() != self._kf_sig:
            self._load_keyframes()
        else:
            self._kf_current = self._current_play_index()
            if self._keyframes_on and self._keyframes:
                self._apply_keyframes_for(self._kf_current)
        if changed or self._flicker_signature() != self._flicker_sig:
            self._load_flicker()
        if changed or self._flicker_signature() != self._holy_sig:
            self._load_holy()
        if changed or self._flicker_signature() != self._qa_sig:
            self._reset_qa()
        self._update_keyframes_ui()
        self._update_qa_ui()

    def _flush_edits(self):
        if self._save_timer.isActive():
            self._save_timer.stop()
        if self._keyframes_on:
            self._flush_keyframe_edit()
        key = self._edit_key
        if not key or not edits.enabled():
            return
        if self._keyframes_on:
            # 关键帧模式下 store.p 只是某一帧的插值结果，不能当全序列基准存
            if self._keyframes:
                self._persist_keyframes()
            return
        if gc.changes(self.store.p):
            edits.save(key, self.store.p)
        else:
            edits.clear(key)

    # ------------------------------------------------------------------ 关键帧
    def _keyframe_capable(self):
        return self.video is None and len(self._play_items) >= 2

    def _keyframe_signature(self):
        return tuple(os.path.basename(p) for p in self._play_items)

    def _current_play_index(self):
        if not self._keyframe_capable():
            return None
        n = len(self._play_items)
        return max(0, min(int(self.player.index), n - 1))

    def _load_keyframes(self):
        sig = self._keyframe_signature()
        self._kf_sig = sig
        self._keyframes = []
        self._keyframes_on = False
        self._kf_files = ([os.path.basename(p) for p in self._play_items]
                          if len(sig) >= 2 else [])
        data = None
        if len(sig) >= 2 and self._edit_key and edits.enabled():
            data = edits.load_keyframes(self._edit_key, self._play_items)
        if data is not None:
            self._keyframes = data["keyframes"]
            self._keyframes_on = bool(data.get("on"))
        self._kf_current = self._current_play_index()
        if self._keyframes_on and self._keyframes:
            self._apply_keyframes_for(self._kf_current)
        self._update_keyframes_ui()

    def _apply_keyframes_for(self, index):
        """把第 index 帧的插值参数送进 store；不改历史之外的任何状态。"""
        if index is None or not (self._keyframes_on and self._keyframes):
            return False
        p = seq.params_at(self.store.p, self._keyframes, index)
        if p == self.store.p:
            return False
        self._kf_applying = True
        try:
            self.store.load(p)
        finally:
            self._kf_applying = False
        return True

    def _commit_keyframe(self, force=False):
        """把当前参数记到当前帧；离开序列前用 force 跳过帧名单校验。"""
        if not (self._keyframes_on and self._keyframe_capable()):
            return
        if not force and self._keyframe_signature() != self._kf_sig:
            return
        i = self._kf_current
        if i is None:
            return
        self._keyframes = seq.upsert(self._keyframes, i, self.store.p)
        self._persist_keyframes()
        self._update_keyframes_ui()

    def _flush_keyframe_edit(self):
        """只有还没落盘的编辑才补记；单纯导航/退出不能凭空多出关键帧。"""
        if not self._kf_timer.isActive():
            return
        self._kf_timer.stop()
        self._commit_keyframe(force=True)

    def _persist_keyframes(self):
        key = self._edit_key
        if not key or not edits.enabled() or not self._kf_files:
            return False
        return edits.save_keyframes(key, {"files": self._kf_files,
                                          "on": self._keyframes_on,
                                          "keyframes": self._keyframes})

    def _freeze_base(self):
        """把当前参数落成全序列基准，供关闭插值后继续使用。"""
        key = self._edit_key
        if not key or not edits.enabled():
            return
        if gc.changes(self.store.p):
            edits.save(key, self.store.p)
        else:
            edits.clear(key)

    def set_keyframes_on(self, on):
        on = bool(on) and self._keyframe_capable()
        if on == self._keyframes_on:
            self._update_keyframes_ui()
            return
        if self._save_timer.isActive():
            self._save_timer.stop()
        if on:
            if not self._kf_files:
                self._kf_files = [os.path.basename(p) for p in self._play_items]
                self._kf_sig = self._keyframe_signature()
            self._kf_current = self._current_play_index()
            self._keyframes_on = True
            if not self._keyframes and self._kf_current is not None:
                self._keyframes = seq.upsert([], self._kf_current,
                                             self.store.p)
            self._persist_keyframes()
            self._apply_keyframes_for(self._kf_current)
            self.status("关键帧插值已启用：改参数会记录到当前帧")
        else:
            self._flush_keyframe_edit()
            self._keyframes_on = False
            self._freeze_base()
            self._persist_keyframes()
            self.status("关键帧插值已关闭：当前参数应用到整个序列")
        self._update_keyframes_ui()
        self._refresh_reference()
        self._render()

    def record_keyframe(self):
        if not self._keyframe_capable():
            self.status("关键帧需要至少两张图片组成的序列")
            return
        if not self._keyframes_on:
            self.set_keyframes_on(True)
            return
        if self._kf_timer.isActive():
            self._kf_timer.stop()
        self._commit_keyframe(force=True)
        self._refresh_reference()
        self.status(f"已记录关键帧：第 {self._kf_current + 1} 帧")

    def remove_keyframe(self):
        if not self._keyframe_capable():
            return
        if self._kf_timer.isActive():
            self._flush_keyframe_edit()
        i = self._current_play_index()
        if seq.at(self._keyframes, i) is None:
            self.status("当前帧不是关键帧")
            return
        self._keyframes = seq.remove(self._keyframes, i)
        if not self._keyframes:
            self._keyframes_on = False
            if self._edit_key:
                edits.clear_keyframes(self._edit_key)
        else:
            self._persist_keyframes()
            self._apply_keyframes_for(i)
        self._update_keyframes_ui()
        self._refresh_reference()
        self._render()
        self.status(f"已删除第 {i + 1} 帧的关键帧")

    def clear_keyframes(self):
        if not self._keyframes:
            return
        self._keyframes = []
        self._keyframes_on = False
        if self._edit_key:
            edits.clear_keyframes(self._edit_key)
        self._freeze_base()
        self._update_keyframes_ui()
        self._render()
        self.status("已清空序列关键帧，当前参数应用到整个序列")

    def _update_keyframes_ui(self):
        if not hasattr(self, "cb_keyframes"):
            return
        capable = self._keyframe_capable()
        n = len(self._keyframes)
        for w in (self.cb_keyframes, self.btn_kf_record):
            w.setEnabled(capable)
        self.cb_keyframes.blockSignals(True)
        self.cb_keyframes.setChecked(capable and self._keyframes_on)
        self.cb_keyframes.blockSignals(False)
        self.btn_kf_remove.setEnabled(capable and n > 0)
        self.btn_kf_clear.setEnabled(capable and n > 0)
        if hasattr(self, "act_kf_on"):
            self.act_kf_on.setEnabled(capable)
            self.act_kf_on.blockSignals(True)
            self.act_kf_on.setChecked(capable and self._keyframes_on)
            self.act_kf_on.blockSignals(False)
            self.act_kf_record.setEnabled(capable)
            self.act_kf_remove.setEnabled(capable and n > 0)
        if not capable:
            self.lb_keyframes.setText("需要至少两张图片的序列")
        elif not n:
            self.lb_keyframes.setText("未记录关键帧")
        else:
            cur = self._kf_current
            here = seq.at(self._keyframes, cur) is not None
            self.lb_keyframes.setText(
                f"{n} 个关键帧 · 第 {cur + 1} 帧"
                + ("（关键帧）" if here else "")
                + ("" if self._keyframes_on else " · 未启用"))
        self.sl_time.marks = [kf["index"] for kf in self._keyframes] \
            if capable else []
        self.sl_time.update()
        # 曲线只有在能逐帧改参数时才允许拖：拖出来的必须是关键帧，否则改的
        # 是全局参数，等于把整段一起挪走
        if hasattr(self, "browser"):
            self.browser.set_curve_editable(capable)
        self._update_curve_corrected()

    def _frame_ev_series(self, n):
        """每帧真正吃到的曝光偏移：关键帧插值 + 去闪烁 + 台阶补偿。

        和 `_batch_params` 那条导出路径用同一套算法，所以白线就是成片里的亮度
        走势。去闪烁/台阶只各校验一次帧名单再直接读 offsets 数组——逐帧走
        `current_deflicker()` 会把 O(n) 的校验做成 O(n²)，几千帧的序列要卡住。
        """
        kfs = self._keyframes if (self._keyframes_on and self._keyframes) else []
        if kfs:
            evs = seq.exposure_track(self.store.p, kfs, n)
        else:
            try:
                base = float(self.store.value("exposure"))
            except (TypeError, ValueError):
                base = 0.0
            evs = [base] * n

        def clamp(v):
            return max(-flicker.MAX_EV, min(flicker.MAX_EV, float(v)))

        if self._deflicker_on and self._deflicker_valid():
            offs = self._deflicker.get("offsets") or []
            for i in range(min(n, len(offs))):
                v = offs[i]
                try:
                    if isinstance(v, (list, tuple)) and len(v) == 3:
                        # 分通道修正取三通道均值：曲线只有一根亮度轴
                        evs[i] += (clamp(v[0]) + clamp(v[1]) + clamp(v[2])) / 3.0
                    else:
                        evs[i] += clamp(v)
                except (TypeError, ValueError):
                    continue

        if self._holy_on and self._holy_valid():
            offs = self._holy.get("offsets") or []
            for i in range(min(n, len(offs))):
                try:
                    evs[i] += clamp(offs[i])
                except (TypeError, ValueError):
                    continue
        return evs

    def _update_curve_corrected(self):
        """把逐帧曝光推给曲线，让它画出第二条"校正后"的线。"""
        if not hasattr(self, "browser"):
            return
        items = self._play_items
        if len(items) < 2:
            self.browser.set_curve_offsets([])
            return
        evs = self._frame_ev_series(len(items))
        # 曲线的横轴是浏览器自己那份 paths（可能含视频、bmp 这类不进播放序列的），
        # 两边来自同一个 media_items，按路径取值即可，别按下标硬贴
        table = dict(zip(items, evs))
        self.browser.set_curve_offsets([table.get(p, 0.0)
                                        for p in self.browser.paths])

    # ---- 拖曲线改曝光 ----
    def _on_curve_begin(self, i):
        """按下曲线：先跳到那一帧——改的必须是你正在看的那一帧。"""
        self._curve_drag = None
        if not self._keyframe_capable():
            return
        if i != self._current_play_index():
            self.select_index(i)
        self._curve_drag = {"i": i, "e0": None, "delta": 0.0}

    def _on_curve_edit(self, i, delta):
        d = self._curve_drag
        if d is None or d["i"] != i or self._current_play_index() != i:
            return
        if abs(delta) < 1e-4:
            return                        # 只按下没拖动：别凭空生出关键帧
        if d["e0"] is None:
            self._curve_seed(i)
            try:
                d["e0"] = float(self.store.value("exposure"))
            except (TypeError, ValueError):
                d["e0"] = 0.0
        d["delta"] = float(delta)
        lo, hi = params.LIMITS["exposure"]
        self.store.set("exposure",
                       round(max(lo, min(hi, d["e0"] + delta)), 2))
        self._update_curve_corrected()

    def _curve_seed(self, i):
        """第一次真的拖出增量才动状态：开插值、首尾下锚点，这一次拖动算一步撤销。"""
        if not self._keyframes_on:
            self.set_keyframes_on(True)
        self._kf_current = i
        n = len(self._play_items)
        for anchor in (0, n - 1):
            if anchor != i and seq.at(self._keyframes, anchor) is None:
                self._keyframes = seq.upsert(self._keyframes, anchor,
                                             self.store.p)
        self._persist_keyframes()
        self.store.begin_edit()

    def _on_curve_end(self):
        d, self._curve_drag = self._curve_drag, None
        if d is None:
            return
        self.store.end_edit()
        self._flush_keyframe_edit()
        self._update_keyframes_ui()
        self._update_curve_corrected()
        if d["e0"] is None:
            return
        self.status(f"第 {d['i'] + 1} 帧曝光 {d['delta']:+.2f} EV："
                    f"已落成 {len(self._keyframes)} 个关键帧，Ctrl+Z 撤销这一次拖动")
        self._render()

    # ------------------------------------------------------------------ 去闪烁
    def photo_items(self):
        """当前列表里的图片（RAW 也算），去闪烁分析只看这些。"""
        return [p for p in self.media_items
                if os.path.splitext(p)[1].lower() in IMG_EXT]

    def _flicker_signature(self):
        return tuple(os.path.basename(p) for p in self._play_items)

    def _load_flicker(self):
        sig = self._flicker_signature()
        self._flicker_sig = sig
        self._deflicker = None
        self._deflicker_on = False
        if len(sig) >= 2 and self._edit_key and edits.enabled():
            data = edits.load_deflicker(self._edit_key, self._play_items)
            if data is not None:
                self._deflicker = data
                self._deflicker_on = True
        self._update_flicker_ui()

    def _deflicker_valid(self):
        d = self._deflicker
        if not d:
            return False
        names = [os.path.basename(p) for p in self._play_items]
        return (d.get("files") == names
                and len(d.get("offsets") or []) == len(names)
                and len(names) >= 2)

    def current_deflicker(self):
        """列表匹配且勾选「应用校正」时才生效。"""
        return self._deflicker if (self._deflicker_on
                                   and self._deflicker_valid()) else None

    def _flicker_ev(self, index=None):
        """当前帧的去闪烁修正 (R,G,B)。

        查表与限幅在 renderq 里 —— 渲染队列渲每一段用的是同一份，不会出现
        预览一个坡、成片另一个坡。
        """
        if index is None:
            index = self.player.index if self._play_items else -1
        return renderq.flicker_ev(self.current_deflicker(), index)

    def _ev_into(self, p, index):
        """把这一帧的去闪烁 + 台阶曝光写进参数副本；没有修正返回原参数。"""
        return renderq.ev_into(p, index, self.current_deflicker(),
                               self.current_holy())

    def _preview_params(self, index=None):
        """当前帧参数：关键帧插值 + 去闪烁曝光；都没启用时直接用原参数。"""
        idx = self.player.index if index is None else index
        try:
            idx = int(idx)
        except (TypeError, ValueError):
            idx = -1
        if self._keyframes_on and self._keyframes:
            return self._ev_into(seq.params_at(self.store.p, self._keyframes,
                                               idx), idx)
        return self._ev_into(self.store.p, idx)

    def set_deflicker(self, data, persist=True):
        self._deflicker = dict(data)
        self._deflicker_on = True
        self._flicker_sig = self._flicker_signature()
        if persist and self._edit_key and edits.enabled():
            edits.save_deflicker(self._edit_key, self._deflicker)
        self._update_flicker_ui()
        self._render()

    def clear_deflicker(self, persist=True):
        self._deflicker = None
        self._deflicker_on = False
        if persist and self._edit_key:
            edits.clear_deflicker(self._edit_key)
        self._update_flicker_ui()
        self._render()

    def _on_flicker_toggle(self, on):
        self._deflicker_on = bool(on) and self._deflicker_valid()
        self._update_flicker_ui()
        self._render()

    def _update_flicker_ui(self):
        if not hasattr(self, "btn_flicker"):
            return
        n = len(self._play_items)
        valid = self._deflicker_valid()
        self.btn_flicker.setEnabled(n >= 2)
        self.cb_flicker.setEnabled(valid)
        self.cb_flicker.blockSignals(True)
        self.cb_flicker.setChecked(valid and self._deflicker_on)
        self.cb_flicker.blockSignals(False)
        if valid:
            offs = self._deflicker.get("offsets") or []
            vals = [x for o in offs for x in (o if isinstance(o, (list, tuple))
                                              else [o])]
            mx = max((abs(v) for v in vals), default=0.0)
            mode = "分通道" if self._deflicker.get("channels") else "整帧亮度"
            if self._deflicker.get("multi"):
                mode += " · 两遍"
            self.lb_flicker.setText(
                f"{len(offs)} 帧 · {mode} · 最大 ±{mx:.2f} EV · "
                + ("已应用" if self._deflicker_on else "未应用"))
        else:
            self.lb_flicker.setText(
                "未分析" if n >= 2 else "需要至少两张图片的序列")
        if hasattr(self, "act_flicker"):
            self.act_flicker.setEnabled(n >= 2)
        self._update_curve_corrected()

    # ------------------------------------------------------------------ 曝光台阶
    def _load_holy(self):
        sig = self._flicker_signature()
        self._holy_sig = sig
        self._holy = None
        self._holy_on = False
        if len(sig) >= 2 and self._edit_key and edits.enabled():
            data = edits.load_holy(self._edit_key, self._play_items)
            if data is not None:
                self._holy = data
                self._holy_on = True
                self.sp_holy_ramp.blockSignals(True)
                self.sp_holy_ramp.setValue(int(data.get("ramp", holly.RAMP)))
                self.sp_holy_ramp.blockSignals(False)
        self._update_holy_ui()
        self._sync_curve_steps()

    def _holy_valid(self):
        d = self._holy
        if not d:
            return False
        names = [os.path.basename(p) for p in self._play_items]
        return (d.get("files") == names and bool(d.get("steps"))
                and len(d.get("offsets") or []) == len(names)
                and len(names) >= 2)

    def current_holy(self):
        return self._holy if (self._holy_on and self._holy_valid()) else None

    def _holy_offset(self, index=None):
        if index is None:
            index = self.player.index if self._play_items else -1
        return renderq.holy_offset(self.current_holy(), index)

    def detect_holy(self):
        items = self.photo_items()
        if len(items) < 2:
            self.warn("曝光台阶需要至少两张图片组成的序列")
            return
        job = holy_ui.DetectJob(
            items, self.browser.means_for(items),
            self.sp_holy_ramp.value())
        self.btn_holy.setEnabled(False)
        if not self.start_job(job, self._holy_done):
            self.btn_holy.setEnabled(True)
            return
        self.status("正在检测曝光台阶…")

    def _holy_done(self, ok, res):
        self.btn_holy.setEnabled(len(self.photo_items()) >= 2)
        if not ok:
            self.status("已取消台阶检测" if res == "cancelled"
                        else f"台阶检测失败：{res}")
            return
        data = {
            "files": res.get("files") or [],
            "steps": res.get("steps") or [],
            "source": res.get("source", "exif"),
            "ramp": int(res.get("ramp", holly.RAMP)),
            "offsets": res.get("offsets") or [],
        }
        self.set_holy(data)
        if data["steps"]:
            self.status(f"检测到 {len(data['steps'])} 处曝光台阶，已应用补偿")
        else:
            self.status("未发现曝光台阶")

    def set_holy(self, data, persist=True):
        self._holy = dict(data)
        self._holy_on = bool(self._holy.get("steps"))
        self._holy_sig = self._flicker_signature()
        ramp = int(self._holy.get("ramp", holly.RAMP))
        self.sp_holy_ramp.blockSignals(True)
        self.sp_holy_ramp.setValue(ramp)
        self.sp_holy_ramp.blockSignals(False)
        if persist and self._edit_key and edits.enabled():
            if self._holy_on:
                edits.save_holy(self._edit_key, self._holy)
            else:
                edits.clear_holy(self._edit_key)
        self._update_holy_ui()
        self._sync_curve_steps()
        self._render()

    def clear_holy(self, persist=True):
        self._holy = None
        self._holy_on = False
        if persist and self._edit_key:
            edits.clear_holy(self._edit_key)
        self._update_holy_ui()
        self._sync_curve_steps()
        self._render()

    def _on_holy_toggle(self, on):
        self._holy_on = bool(on) and self._holy_valid()
        self._update_holy_ui()
        self._render()

    def _on_holy_ramp(self, value):
        if not self._holy or not self._holy.get("steps"):
            return
        n = len(self._holy.get("files") or [])
        steps = [(int(s[0]), float(s[1])) for s in self._holy["steps"]]
        self._holy["ramp"] = int(value)
        self._holy["offsets"] = holly.ramp_offsets(
            steps, n, value).tolist()
        if self._edit_key and edits.enabled() and self._holy_valid():
            edits.save_holy(self._edit_key, self._holy)
        self._update_holy_ui()
        self._render()

    def _update_holy_ui(self):
        if not hasattr(self, "btn_holy"):
            return
        n = len(self._play_items)
        cap = n >= 2
        self.btn_holy.setEnabled(cap)
        if hasattr(self, "act_holy"):
            self.act_holy.setEnabled(cap)
        valid = self._holy_valid()
        self.sp_holy_ramp.setEnabled(bool(self._holy and self._holy.get("steps")))
        self.btn_holy_clear.setEnabled(bool(self._holy and self._holy.get("steps")))
        self.cb_holy.setEnabled(valid)
        self.cb_holy.blockSignals(True)
        self.cb_holy.setChecked(valid and self._holy_on)
        self.cb_holy.blockSignals(False)
        if valid:
            steps = self._holy["steps"]
            mx = max(abs(float(s[1])) for s in steps)
            src = "EXIF" if self._holy.get("source") == "exif" else "亮度推断"
            self.lb_holy.setText(
                f"{len(steps)} 处台阶 · {src} · 最大 ±{mx:.1f} EV · "
                + ("已应用" if self._holy_on else "未应用"))
        elif self._holy and self._holy.get("steps"):
            self.lb_holy.setText("序列变了，重新检测台阶")
        else:
            self.lb_holy.setText(
                "未检测" if cap else "需要至少两张图片的序列")
        self._update_curve_corrected()

    def _sync_curve_steps(self):
        if not hasattr(self, "browser"):
            return
        steps = []
        if self._holy_valid():
            n = len(self._play_items)
            steps = [(int(s[0]), float(s[1]),
                      str(s[2]) if len(s) > 2 else "")
                     for s in self._holy["steps"] if 0 <= int(s[0]) < n]
        self.browser.set_steps(steps)

    # ------------------------------------------------------------------ 序列体检
    def open_qa(self):
        items = self.photo_items()
        if len(items) < 2:
            self.warn("序列体检需要至少两张图片组成的序列")
            return
        if self._qa_dlg is not None:
            self._qa_dlg.raise_()
            self._qa_dlg.activateWindow()
            return
        dlg = qa_ui.QaDialog(self, items)
        dlg.finished.connect(lambda _=0: setattr(self, "_qa_dlg", None))
        self._qa_dlg = dlg
        dlg.show()

    def qa_excluded(self):
        """体检勾选要排除的帧（normcase 路径集合）。"""
        return set(self._qa_excluded)

    def apply_qa(self, items):
        """去掉体检勾选排除的帧；按路径记，列表重排也不会错位。"""
        if not self._qa_excluded:
            return list(items)
        return [p for p in items if os.path.normcase(os.path.normpath(p))
                not in self._qa_excluded]

    def set_qa_excluded(self, paths):
        self._qa_excluded = {os.path.normcase(os.path.normpath(p))
                             for p in paths if p}
        self._update_qa_ui()

    def set_qa_findings(self, items, findings):
        """记住每帧的异常类型：图库「只看异常」靠它，不再重跑一遍体检。"""
        table = {}
        for i, kind, _val, _msg in findings:
            if 0 <= i < len(items):
                key = os.path.normcase(os.path.normpath(items[i]))
                table.setdefault(key, []).append(kind)
        self._qa_kinds = {k: tuple(v) for k, v in table.items()}
        for b in (self.browser, self.grid_browser):
            b.set_qa_kinds(self._qa_kinds)
        self._sync_qa_filter_ui()
        self._update_qa_ui()

    def clear_qa(self):
        dlg = self._qa_dlg
        if dlg is not None:
            dlg.uncheck_all()
        self._qa_excluded = set()
        self._update_qa_ui()

    def _reset_qa(self):
        """换素材就丢掉排除：帧号与路径都不再对应上次那份测量。"""
        self._qa_excluded = set()
        self._qa_kinds = {}
        self._qa_sig = self._flicker_signature()
        for b in (self.browser, self.grid_browser):
            b.set_qa_kinds({})
        dlg = self._qa_dlg
        if dlg is not None:
            dlg.invalidate()
        self._sync_qa_filter_ui()
        self._update_qa_ui()

    def _update_qa_ui(self):
        if not hasattr(self, "btn_qa"):
            return
        n = len(self._qa_excluded)
        found = len(self._qa_kinds)
        self.btn_qa.setEnabled(len(self.photo_items()) >= 2)
        self.btn_qa_clear.setEnabled(bool(n))
        self.lb_qa.setText(
            f"已排除 {n} 帧，批量导出会跳过" if n else
            (f"发现 {found} 帧异常，可在图库「只看异常」筛出来" if found else
             ("未体检" if self.btn_qa.isEnabled()
              else "需要至少两张图片的序列")))

    def open_deflicker(self):
        items = self.photo_items()
        if len(items) < 2:
            self.warn("去闪烁需要至少两张图片组成的序列")
            return
        if self._deflicker_dlg is not None:
            self._deflicker_dlg.raise_()
            self._deflicker_dlg.activateWindow()
            return
        dlg = flicker_ui.DeflickerDialog(self, items)
        dlg.finished.connect(lambda _=0: setattr(self, "_deflicker_dlg", None))
        self._deflicker_dlg = dlg
        dlg.show()

    # ------------------------------------------------------------------ 渲染
    def _on_params_changed(self, source):
        for r in self.rows:
            r.sync()
        self.hsl.sync()
        self.curve.sync()
        self.levels.update()
        self.masks.sync()
        self.view.set_masks(self.store.p.get("masks"))
        self._sync_preset_btns()
        self._update_history()
        if self.work is not None and not self._kf_applying:
            if not self.timer.isActive():
                self.timer.start()
            self.idle.start()
            if self._edit_key and edits.enabled():
                if self._keyframes_on and self._keyframe_capable():
                    self._kf_timer.start()
                else:
                    self._save_timer.start()

    def _on_mask_drag(self, i, j, kind, a, b):
        if kind == "body":
            self.store.mask_shift(i, a, b, j=j)
        else:
            self.store.mask_move(i, a, b, end=(kind == "b"), j=j)

    def _on_mask_drag_state(self, on):
        """一次拖动算一步：按下存快照，松手收进历史。"""
        if on:
            self.store.begin_edit()
        else:
            self.store.end_edit()

    def _render(self, full=True):
        """full=False 只出草图：拖动时用 900 长边，像素少 4 倍。"""
        if self.work is None:
            return
        img = self.work if full else (self.proxy if self.proxy is not None
                                      else self.work)
        self.grader.submit(img, self._preview_params(), fast=not full)

    def _on_render(self, res):
        self._render_failed = False
        self.view.set_result(res.image)
        self.scopes.set_image(res.array)
        self.levels.set_hist(res.array)

    def _on_render_failed(self, msg):
        """渲染线程报错时继续留在原地：状态栏给原因，弹窗每次故障只弹一次。"""
        self.status("预览渲染失败：" + msg)
        if self._render_failed:
            return
        self._render_failed = True
        self.warn("这一帧的调色参数无法渲染：\n" + msg, "预览失败")

    # ------------------------------------------------------------------ 历史
    def undo(self):
        self.store.undo()

    def redo(self):
        self.store.redo()

    def auto(self):
        """一键自动：白平衡 + 曝光/影调，结果仍可继续手调、可撤销。

        auto_params 会把「再动就过曝/死黑」的提案自己收掉，所以这里可能出现
        影调全 0 的情况：那不是没生效，是这张图本来就不用动，状态栏说清楚。
        """
        if self.work is None:
            self.warn("先打开一张图片或视频")
            return
        q = gc.auto_params(self.work)
        self.store.merge(q, source="auto")
        if any(q[k] for k in ("exposure", "contrast", "highlights",
                              "shadows", "whites", "blacks")):
            self.status("自动校正：曝光 {:+.2f} EV · 色温 {:+.0f} · 对比 {:+.0f}"
                        .format(q["exposure"], q["temperature"], q["contrast"]))
        else:
            self.status("自动校正：影调已在安全范围，只校白平衡（色温 {:+.0f}）"
                        .format(q["temperature"]))

    def _update_history(self):
        self.btn_undo.setEnabled(self.store.can_undo())
        self.btn_redo.setEnabled(self.store.can_redo())

    def _copy_settings(self):
        self._clip = copy.deepcopy(self.store.p)
        self.status("已复制当前调色设置")

    def _paste_settings(self):
        if self._clip is None:
            self.status("剪贴板为空")
            return
        self.store.push()
        self.store.restore(copy.deepcopy(self._clip))

    # ------------------------------------------------------------------ 对比
    def toggle_compare(self, on=None):
        self.view.set_compare(on if on is not None else not self.view.compare)
        self.act_compare.setChecked(self.view.compare)

    # ------------------------------------------------------------------ 参考帧
    # 调长序列时最需要的是"上一关键帧长什么样"：左侧钉住参考帧、右侧看当前，
    # 一眼判断曝光/色温有没有跳。参考帧渲染在 900 长边的代理上，够看且不卡手。
    def _ref_capable(self):
        return self.video is None and len(self._play_items) >= 2

    def _sync_ref_actions(self):
        ok = self._ref_capable()
        self.act_ref.setEnabled(ok)
        self.act_ref_lock.setEnabled(ok and self.src_path is not None)
        self.act_ref_clear.setEnabled(self._ref_path is not None)
        if not ok and self.act_ref.isChecked():
            self.act_ref.setChecked(False)

    def _auto_ref_index(self):
        i = self._current_play_index()
        if i is None or not (0 <= i < len(self._play_items)):
            return None
        if self._keyframes_on:
            prev = [k for k in seq.indices(self._keyframes) if k < i]
            if prev:
                return max(prev)
        if i > 0:
            return i - 1
        return 1 if len(self._play_items) > 1 else None

    def _show_reference(self):
        if not self._ref_path or self._ref_path not in self._play_items:
            return False
        idx = self._play_items.index(self._ref_path)
        p_ref = (seq.params_at(self.store.p, self._keyframes, idx)
                 if self._keyframes_on and self._keyframes else self.store.p)
        sig = (self._ref_path,
               repr(sorted(p_ref.items(), key=lambda kv: kv[0])))
        if sig != self._ref_sig:
            im = pipeline.load_u8(self._ref_path)
            if im is None:
                return False
            f = pipeline.fit_long(im, pipeline.PROXY_LONG).astype(np.float32) / 255.0
            out = gc.apply(f, p_ref) if gc.changes(p_ref) else f
            self._ref_cache = pipeline.to_qimage_u8(
                (np.clip(out, 0.0, 1.0) * 255.0 + 0.5).astype(np.uint8))
            self._ref_sig = sig
        self.view.set_reference(self._ref_cache)
        return True

    def toggle_reference(self, on):
        on = bool(on)
        if on:
            if not self._ref_capable():
                self.act_ref.setChecked(False)
                self.status("参考帧对比至少需要两张图片")
                return
            self._compare_before_ref = self.view.compare
            if not self._ref_path or self._ref_path not in self._play_items:
                j = self._auto_ref_index()
                if j is None:
                    self.act_ref.setChecked(False)
                    return
                self._ref_path = self._play_items[j]
                self._ref_manual = False
                self._ref_cache = self._ref_sig = None
            if not self._show_reference():
                self.act_ref.setChecked(False)
                self.status("参考帧读取失败：" + self._ref_path)
                return
            self.act_compare.setChecked(True)
            self.view.set_compare(True)
            i = self._play_items.index(self._ref_path) + 1
            self.status(f"参考帧：第 {i}/{len(self._play_items)} 张（左侧）")
        else:
            self.view.set_reference(None)
            self.act_compare.setChecked(self._compare_before_ref)
            self.view.set_compare(self._compare_before_ref)
        self._sync_ref_actions()

    def lock_reference(self):
        i = self._current_play_index() if self._ref_capable() else None
        if i is None or not (0 <= i < len(self._play_items)):
            return
        self._ref_path = self._play_items[i]
        self._ref_manual = True
        self._ref_cache = self._ref_sig = None
        if not self.act_ref.isChecked():
            self.act_ref.setChecked(True)
        else:
            self._show_reference()
        self.status(f"已锁定参考帧：第 {i + 1}/{len(self._play_items)} 张")
        self._sync_ref_actions()

    def clear_reference(self):
        self._ref_path = None
        self._ref_manual = False
        self._ref_cache = self._ref_sig = None
        if self.act_ref.isChecked():
            self.act_ref.setChecked(False)
        self.view.set_reference(None)
        self.status("已清除参考帧")
        self._sync_ref_actions()

    def _refresh_reference(self):
        if self.act_ref.isChecked():
            self._show_reference()

    # ------------------------------------------------------------------ 导出
    def _out_dir_for(self, path):
        d = os.path.join(os.path.dirname(path), "graded")
        os.makedirs(d, exist_ok=True)
        return d

    def export_image(self):
        if self.work is None or not self.src_path:
            self.warn("先打开一张图片")
            return
        fmt = self.ex_fmt.currentText().upper()
        ext = exporter.EXPORT_FORMATS.get(fmt, ".jpg")
        base = os.path.splitext(os.path.basename(self.src_path))[0]
        default = os.path.join(self._out_dir_for(self.src_path), base + ext)
        dst, _ = QtWidgets.QFileDialog.getSaveFileName(self.shell, "导出图片",
                                                      default, f"*{ext}")
        if not dst:
            return
        QtWidgets.QApplication.setOverrideCursor(QtCore.Qt.CursorShape.WaitCursor)
        try:
            im = exporter.load_full(self.src_path)
        finally:
            QtWidgets.QApplication.restoreOverrideCursor()
        if im is None:
            self.status("导出失败：读不到原图 " + self.src_path)
            return
        try:
            out = np.clip(gc.apply(im, self._preview_params()), 0, 1)
            ok = exporter.save_image(dst, out, self.ex_q.value(),
                                     self._export_bit())
        except Exception as e:
            self.status("导出失败：" + str(e))
            return
        if ok:
            self.status("已导出 " + dst)
        else:
            self.status("导出失败：写不进 " + dst)

    def export_batch(self):
        items = self.photo_items()
        if not items:
            self.warn("当前列表没有图片")
            return
        # 图库筛过就只导筛中的那些；筛是视图层的，media_items 仍是全量
        if self.ex_filtered.isChecked() and self.grid_browser.filter_spec():
            photos = {os.path.normcase(os.path.normpath(p)) for p in items}
            items = [p for p in self.grid_browser.filtered_paths()
                     if os.path.normcase(os.path.normpath(p)) in photos]
            if not items:
                self.warn("当前筛选一张图片都没留下，先放宽筛选条件")
                return
        # 体检勾选排除的帧跳过；排除按路径记，重排列表也不会错位
        keep = self.apply_qa(items)
        if not keep:
            self.warn("体检把整段序列都排除了，先清除排除再导出")
            return
        skipped = len(items) - len(keep)
        d = QtWidgets.QFileDialog.getExistingDirectory(
            self.shell, "选择输出文件夹", self._out_dir_for(items[0]))
        if not d:
            return
        fmt = self.ex_fmt.currentText()
        fmt = fmt if fmt in exporter.EXPORT_FORMATS else "JPG"
        bit = self._export_bit()
        resume = self._ask_resume(d, fmt, bit, len(keep))
        job = BatchExport(keep, d, self.store.p, self.ex_q.value(),
                          fmt, bit, params_at=self._batch_params(),
                          resume=resume)
        if skipped:
            self.status(f"体检排除了 {skipped} 帧")
        self.start_export(job,
                          lambda ok, res: self._batch_done(ok, res, d, job))

    def _ask_resume(self, out_dir, fmt, bit, total):
        """有上次没导完的进度就问一句；返回 True 表示接着导。

        按钮自己写：Qt 的 Yes/No 在这套中文界面里没人看得懂哪个是续跑。
        """
        info = renderq.read_progress(out_dir, fmt, bit)
        if not info:
            return True
        done = min(len(info["done"]), total)
        box = QtWidgets.QMessageBox(self.shell)
        box.setWindowTitle("导出整个文件夹")
        box.setIcon(QtWidgets.QMessageBox.Icon.Question)
        box.setText(f"上次导出未完成：已完成 {done} 帧。")
        box.setInformativeText(
            f"接着只补剩下的 {max(0, total - done)} 帧，"
            f"还是重新导出全部 {total} 帧？")
        go_on = box.addButton(f"继续导剩下的 "
                              f"{max(0, total - done)} 帧",
                              QtWidgets.QMessageBox.ButtonRole.AcceptRole)
        box.addButton("全部重新导出",
                      QtWidgets.QMessageBox.ButtonRole.DestructiveRole)
        box.setDefaultButton(go_on)
        box.exec()
        return box.clickedButton() is go_on

    def _batch_params(self):
        """批量导出：按文件位置取关键帧插值，再叠加去闪烁与台阶曝光。"""
        kfs = self._keyframes if (self._keyframes_on and self._keyframes) else []
        return renderq.params_at(self._play_items, self.store.p, kfs,
                                 self.current_deflicker(), self.current_holy())

    def _batch_done(self, ok, res, out_dir, job=None):
        if ok:
            extra = ""
            if job is not None and job.skipped:
                extra = f"，续跑跳过 {job.skipped} 帧"
            self.status(f"批量导出 {len(res)} 张 → {out_dir}{extra}")
        elif isinstance(res, str):
            self.status("批量导出中断：" + res + " · 进度已记下，下次接着导")
        elif isinstance(res, list):
            # 部分帧失败（坏图/写不进）：成功的已经落盘，进度也留着，下次只补剩下的
            self.status(f"批量导出 {len(res)} 张，有帧失败 → {out_dir}"
                        " · 进度已记下，下次接着导")
        else:
            self.status("批量导出中断：" + str(res))

    def export_sheet(self):
        """联系表：均匀抽帧，每格左原图右调色后，一张 PNG 看整段前后差别。

        标签用播放序列里的帧号，逐帧参数也按路径取 —— 体检排除过几帧、
        或者图库筛过，抽出来的格子仍然对得上原来的位置。
        """
        items = self._play_items or self.media_items
        if len(items) < 2:
            self.warn("联系表要一整段序列，至少两张图片")
            return
        pool = self.apply_qa(items)
        if len(pool) < 2:
            self.warn("体检把可抽的帧都排除了，先清除排除再出联系表")
            return
        picks = exporter.sample_indices(len(pool),
                                        self.ex_sheet_n.currentData())
        table = {os.path.normcase(os.path.normpath(p)): i
                 for i, p in enumerate(items)}
        chosen = [(table.get(os.path.normcase(os.path.normpath(pool[i])), i),
                   pool[i]) for i in picks]
        folder = os.path.dirname(items[0])
        dst, _ = QtWidgets.QFileDialog.getSaveFileName(
            self.shell, "导出联系表",
            os.path.join(folder,
                         os.path.basename(folder) + "_联系表.png"),
            "PNG 图片 (*.png)")
        if not dst:
            return
        if not dst.lower().endswith(".png"):
            dst += ".png"
        job = ContactSheet(chosen, dst, self.store.p, cell=SHEET_CELL,
                           cols=self.ex_sheet_cols.currentData(),
                           params_at=self._batch_params())
        self.start_export(
            job, lambda ok, res: self._sheet_done(ok, res, len(chosen)))

    def _sheet_done(self, ok, res, want):
        if ok:
            self.status(f"联系表 {want} 帧 · 每格左原图右调色后 → {res}")
        else:
            self.status("联系表没写成：" + str(res))

    # ------------------------------------------------------------- 渲染队列
    def _queue_fps(self):
        m = re.search(r"\d+(\.\d+)?", self.q_fps.currentText())
        return float(m.group(0)) if m else 0.0

    def _queue_side(self):
        m = re.search(r"\d+", self.q_side.currentText())
        return int(m.group(0)) if m else 0

    def _queue_out(self, folder, fps, side):
        """默认成片名跟着目录走：…/lr3 -> …/lr3_120fps_3840.mp4。

        选"16:9 + 原始（两版）"时这里是无后缀的 stem，出片时再分两条。
        """
        name = os.path.basename(folder) or "timelapse"
        return os.path.join(os.path.dirname(folder),
                            f"{name}_{fps:g}fps_{side}.mp4")

    def _queue_label(self, it):
        mark = {renderq.PENDING: "·", renderq.DONE: "✓",
                renderq.FAILED: "✗"}[it["status"]]
        n = it["frames"] if it["frames"] > 0 else "?"
        return (f"{mark} {os.path.basename(it['folder'])}  {n} 帧  "
                f"{it['fps']:g}fps {it['long_side']} {it['aspect']}")

    def _queue_refresh(self):
        self.lb_queue.clear()
        todo = 0
        for it in self._queue:
            row = QtWidgets.QListWidgetItem(self._queue_label(it))
            tip = it["out"]
            if it["note"]:
                tip += "\n" + it["note"]
            row.setToolTip(tip)
            self.lb_queue.addItem(row)
            if it["status"] != renderq.DONE:
                todo += 1
        self.btn_queue.setEnabled(todo > 0)

    def queue_add(self):
        """把屏幕上这段序列按现在的成片规格排进队列。"""
        items = self._play_items or self.media_items
        if self.video is not None or len(items) < 2:
            self.warn("队列排的是图片序列：先在图库里打开一段（至少两帧）",
                      "渲染队列")
            return
        dirs = {os.path.dirname(os.path.normpath(p)) for p in items}
        if len(dirs) != 1:
            self.warn("这一段跨了好几个目录，队列只能按整个目录排", "渲染队列")
            return
        folder = dirs.pop()
        self._flush_edits()          # 队列读旁车，先把屏幕上这套参数落盘
        it = renderq.coerce({
            "folder": folder,
            "out": self._queue_out(folder, self._queue_fps(),
                                   self._queue_side()),
            "frames": len(items),
            "fps": self._queue_fps(),
            "long_side": self._queue_side(),
            "aspect": dict(ASPECT_CHOICES)[self.q_aspect.currentText()],
            "codec": dict(CODEC_CHOICES)[self.q_codec.currentText()],
            "preset": self.ex_preset.currentData(),
            "crf": int(self.ex_crf.value()),
            "keep_frames": self.q_keep.isChecked(),
        })
        if it is None:
            self.warn("帧率要 ≥1、长边要 ≥64，成片规格还不完整", "渲染队列")
            return
        self._queue = renderq.put(self._queue, it)
        renderq.save(self._queue)
        self._queue_refresh()
        self.status(f"已排进队列：{os.path.basename(folder)} · "
                    f"{it['frames']} 帧")

    def queue_remove(self):
        sel = {i.row() for i in self.lb_queue.selectedIndexes()}
        if not sel:
            self.status("先在队列里选中要移出的行")
            return
        self._queue = [r for k, r in enumerate(self._queue) if k not in sel]
        renderq.save(self._queue)
        self._queue_refresh()

    def queue_clear(self):
        """只清队列这些行：已经渲出来的帧和成片一个都不动。"""
        if not self._queue:
            return
        self._queue = []
        renderq.save(self._queue)
        self._queue_refresh()
        self.status("队列已清空（渲好的文件没动）")

    def _queue_plan(self, it, index, total):
        """一行队列 → 这次渲染要用的东西；(None, 原因) 表示这段渲不了。

        参数、关键帧、去闪烁、曝光台阶全从**它自己目录的旁车**现读：队列的
        意义是"整段按它自己那套调色出片"，跟屏幕上当前选没选中它无关，
        所以这里不碰 self.store，也不碰任何控件。
        """
        folder = it["folder"]
        if not os.path.isdir(folder):
            return None, "目录不存在"
        try:
            s = video.scan_series(folder, allow_gaps=True)
        except video.SeriesError as e:
            return None, str(e)
        sources = renderq.source_paths(folder, s)
        if len(sources) < 2:
            return None, "目录里不足两帧"
        key = renderq.key_for(folder)
        base = edits.load(key) if edits.enabled() else None
        if base is None:
            base = gc.defaults()
        kfs, flick, holy = [], None, None
        if edits.enabled():
            data = edits.load_keyframes(key, sources)
            if data and data.get("on"):
                kfs = data["keyframes"]
            flick = edits.load_deflicker(key, sources)
            holy = edits.load_holy(key, sources)
        w, h = imio.image_size(sources[0])
        return {"item": dict(it, frames=len(sources)),
                "index": index + 1, "total": total,
                "sources": sources, "params": base,
                "params_at": renderq.params_at(sources, base, kfs, flick, holy),
                "sig": renderq.sig_of(base, kfs, flick, holy),
                "quality": renderq.FRAME_QUALITY, "w": w, "h": h}, None

    def render_queue(self):
        if not self.shell.ff:
            self.warn("未找到 ffmpeg，渲不出成片。\n请在运行本工具的 Python "
                      "环境里 pip install imageio-ffmpeg。", "渲染队列")
            return
        if not edits.enabled():
            self.warn("队列靠参数旁车记住每一段各自的调色：请先在「设置」里"
                      "打开参数自动保存，再排一次队。", "渲染队列")
            return
        todo = [r for r in self._queue if r["status"] != renderq.DONE]
        if not todo:
            self.status("队列里没有待渲的段（队列为空，或都渲完了）")
            return
        self._flush_edits()          # 屏幕上这套参数得先落盘，队列才读得到
        plans, why0 = [], ""
        for k, it in enumerate(todo):
            plan, why = self._queue_plan(it, k, len(todo))
            if plan is None:
                it["status"], it["note"] = renderq.FAILED, why
                why0 = why0 or why
                continue
            it["status"], it["note"] = renderq.PENDING, ""
            it["frames"] = plan["item"]["frames"]   # 列表里的帧数按实况更新
            plans.append(plan)
        renderq.save(self._queue)
        self._queue_refresh()
        if not plans:
            self.status(f"队列里 {len(todo)} 段都渲不了：{why0}")
            return
        job = renderq.RenderQueue(plans, self.shell.ff)
        self.start_export(job,
                          lambda ok, res: self._queue_done(ok, res, job))

    def _queue_done(self, ok, res, job):
        table = {os.path.normcase(f): (done, note)
                 for f, done, note, _outs in job.results}
        n_ok = 0
        for it in self._queue:
            got = table.get(os.path.normcase(it["folder"]))
            if not got:
                continue
            it["status"] = renderq.DONE if got[0] else renderq.FAILED
            it["note"] = got[1]
            n_ok += 1 if got[0] else 0
        renderq.save(self._queue)
        self._queue_refresh()
        if ok:
            self.status(f"队列渲成 {n_ok} 段 · {res[0]}")
        elif res == "cancelled":
            self.status(f"队列已取消（渲成 {n_ok} 段）· "
                        "剩下的进度都记下了，再点一次接着渲")
        else:
            self.status(f"队列没全部渲成：{res} · 展开日志看原因")

    def export_cube(self):
        dst, _ = QtWidgets.QFileDialog.getSaveFileName(
            self.shell, "导出 3D LUT",
            os.path.join(os.path.dirname(self.src_path or "."), "grade.cube"),
            "*.cube")
        if not dst:
            return
        gc.write_cube(dst, self.store.p, size=int(self.ex_lut.currentText()))
        self.status("已导出 " + dst)

    def import_xmp(self):
        src, _ = QtWidgets.QFileDialog.getOpenFileName(
            self.shell, "导入 Lightroom 预设", xmp.default_preset_dir(),
            "XMP 预设 (*.xmp)")
        if not src:
            return
        try:
            p = xmp.read_xmp(src)
        except Exception as e:
            self.warn("读不进这个预设：%s" % e, "导入 XMP")
            return
        self.store.merge(p, source="preset")
        name = os.path.splitext(os.path.basename(src))[0]
        self.status("已导入 " + name)
        if p.notes:
            QtWidgets.QMessageBox.information(
                self.shell, "导入 XMP", "\n".join(p.notes))

    def export_xmp(self):
        base = os.path.splitext(os.path.basename(self.src_path or "preset"))[0]
        dst, _ = QtWidgets.QFileDialog.getSaveFileName(
            self.shell, "导出 XMP 预设",
            os.path.join(os.path.dirname(self.src_path or "."), base + ".xmp"),
            "*.xmp")
        if not dst:
            return
        try:
            notes = xmp.write_xmp(dst, self.store.p)
        except Exception as e:
            self.warn("写不出这个预设：%s" % e, "导出 XMP")
            return
        self.status("已导出 " + dst)
        if notes:
            QtWidgets.QMessageBox.information(
                self.shell, "导出 XMP",
                "预设已导出，但有几项 Lightroom 里没有对应：\n\n"
                + "\n".join(notes))

    def export_video(self):
        if not self.video or not self._ff:
            self.warn("先打开一个视频")
            return
        base = os.path.splitext(os.path.basename(self.src_path))[0]
        dst, _ = QtWidgets.QFileDialog.getSaveFileName(
            self.shell, "导出视频",
            os.path.join(self._out_dir_for(self.src_path), base + "_graded.mp4"),
            "*.mp4")
        if not dst:
            return
        dur = max(0.01, self.video["dur"])
        p = self.store.p
        lum = None
        if gc.masks_use_range(p):
            # 视频里每帧亮度不同，静态权重图只能用首帧亮度近似；
            # 导出面板已注明这一点，避免用户把它当成逐帧亮度蒙版。
            frame = os.path.join(self.tmp_dir(), "lum0.png")
            if ffmod.extract_frame(self._ff, self.src_path, 0.0, frame):
                im = pipeline.load_u8(frame)
                if im is not None:
                    lum = im.astype(np.float32) / 255.0
        cmd, cwd = exporter.build_video_cmd(
            self._ff, self.src_path, dst, p,
            self.video["w"], self.video["h"], self.video["fps"],
            self.ex_crf.value(), self.ex_preset.currentData(), self.tmp_dir(),
            lum=lum)
        job = jobs.Job([(None, cmd, jobs.Meter("time", dur), dst, cwd)])
        self.start_export(job, lambda ok, res: self._video_done(dst, ok, res))

    def _video_done(self, dst, ok, res):
        if ok:
            self.status(f"已导出 {dst}")
        elif res == "cancelled":
            self.status("已取消，未完成的文件已清除")
        else:
            self.status("视频导出失败")
            self.warn("\n".join(self._job_log[-8:]) or str(res), "导出失败")

    def start_export(self, job, on_done):
        """导出统一走外壳：进度条与取消按钮全局唯一，日志留在内存供排错。"""
        self._job_log = []
        job.log.connect(self._on_job_log)
        if self.start_job(job, lambda ok, res: self._export_done(on_done, ok, res)):
            self._set_busy(True)

    def _on_job_log(self, text):
        # 用方法而不是 list.append 接信号：跨线程时 QObject 槽才会排队，
        # 直接连普通可调用对象会在工作线程里改主线程的列表。
        self._job_log.append(text)

    def _export_done(self, cb, ok, res):
        self._set_busy(False)
        cb(ok, res)

    def _set_busy(self, busy):
        for b in (self.btn_img, self.btn_batch, self.btn_vid, self.btn_sheet,
                  self.btn_q_add, self.btn_q_del, self.btn_q_clear,
                  self.btn_queue):
            b.setEnabled(not busy)
        # 队列按钮的"能点"还要看有没有待渲的行，交给 _queue_refresh 判定
        if not busy:
            self._queue_refresh()

    def tmp_dir(self):
        """抽帧预览与导出的工作目录：用到才建，免得每次开机都在 TEMP 里留个空壳。"""
        if self._tmp is None:
            self._tmp = tempfile.mkdtemp(prefix="grader_")
        return self._tmp

    # ------------------------------------------------------------------ 收尾
    def shutdown(self):
        self._flush_edits()
        self.player.shutdown()
        self.grader.shutdown()
        self.browser.shutdown()
        self.grid_browser.shutdown()
        if self._tmp:
            shutil.rmtree(self._tmp, ignore_errors=True)
