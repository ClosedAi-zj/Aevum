# -*- coding: utf-8 -*-
"""合成模块：图片序列 -> 延时视频。

acceptance/reference/gui/timelapse.py（tkinter）的 Qt 移植，控件、默认值与校验文案保持一致。
序列识别与命令拼装仍复用 acceptance/reference/video.py；进度、取消、半成品清理走 acceptance/reference/gui/job.py。
成片写完会通过 Material 通知裁剪与调色台，不用手动再选一遍文件。
"""
import os
import re

from PySide6 import QtCore, QtWidgets

from reference import ff as ffmod
from reference import imio, video
from reference.gui import hints
from reference.gui import job as jobs
from reference.gui import widgets
from reference.gui.module import Module
from reference.gui.workbench import APP_TITLE

FPS_CHOICES = ["120", "100", "60", "50", "30", "25", "24"]
SIDE_CHOICES = ["3840 (UHD 4K)", "4096 (DCI 4K)", "2560 (2.5K)",
                "1920 (1080p)", "1280 (720p)"]
ASPECT_CHOICES = [("16:9 + 原始（两版）", "both"),
                  ("16:9  UHD 4K 3840x2160", "16:9"),
                  ("1.90:1  DCI 4K 4096x2160", "1.90:1"),
                  ("3:2", "3:2"), ("4:3", "4:3"),
                  ("原始比例（不裁切）", "none")]
CODEC_CHOICES = [("HEVC / x265（4K 高帧率必需）", "hevc"),
                 ("H.264 / x264（兼容性优先）", "h264")]
PRESET_CHOICES = ffmod.PRESET_LABELS
# CRF 越大越糊，这点反直觉，必须在界面上说出来
CRF_WORDS = ((17, "极高"), (21, "推荐·视觉无损"), (26, "一般"), (31, "偏软"),
             (99, "明显压缩"))


def crf_word(v):
    for lim, w in CRF_WORDS:
        if v <= lim:
            return w
    return "明显压缩"


class Assemble(Module):
    name = "合成"

    def __init__(self, shell):
        super().__init__(shell)
        self.series = None
        self.out_manual = False       # 用户手改过输出名就不再自动覆盖
        self._last_out = None
        self._stage = ""
        self._run_info = None       # 这次合成的参数定格，结果卡用它讲数字

    def top_hint(self):
        """顶栏只报状态：多少张 + 走到哪一步；每个旋钮的说明常驻在控件下面。"""
        if not self.series:
            return ""
        n = self.series["count"]
        return f"{n} 张 · {self._stage}" if self._stage else f"{n} 张"

    # ------------------------------------------------------------------ UI
    def build(self):
        page = QtWidgets.QWidget()
        v = QtWidgets.QVBoxLayout(page)
        v.setContentsMargins(14, 12, 14, 12)
        v.setSpacing(10)

        top = QtWidgets.QHBoxLayout()
        top.setSpacing(8)
        self.btn_dir = widgets.icon_button(
            "folder-open", "选择图片序列目录（Ctrl+Shift+O）")
        self.btn_dir.clicked.connect(self.choose_dir)
        self.ed_dir = QtWidgets.QLineEdit()
        self.ed_dir.setPlaceholderText("图片序列目录；把文件夹拖进窗口也可以")
        self.btn_detect = widgets.icon_button(
            "refresh", "重新检测这个目录里的序列")
        self.btn_detect.clicked.connect(self.detect)
        top.addWidget(self.btn_dir)
        top.addWidget(self.ed_dir, 1)
        top.addWidget(self.btn_detect)
        v.addLayout(top)

        self.lb_info = QtWidgets.QLabel("（未选择目录）")
        self.lb_info.setObjectName("Info")
        self.lb_info.setWordWrap(True)
        v.addWidget(self.lb_info)

        body = QtWidgets.QHBoxLayout()
        body.setSpacing(12)

        left = QtWidgets.QVBoxLayout()
        left.setSpacing(10)
        self.preview = widgets.FramePreview(
            empty="选择序列目录后在这里预览首帧\n也可以把整个文件夹拖进窗口")
        left.addWidget(self.preview, 3)
        self.log_view = QtWidgets.QPlainTextEdit()
        self.log_view.setObjectName("Log")
        self.log_view.setReadOnly(True)
        self.log_view.setPlaceholderText(
            "参数、每段进度与校验结果写在这里。")
        self.log_view.setLineWrapMode(
            QtWidgets.QPlainTextEdit.LineWrapMode.NoWrap)
        self.log_view.setMinimumHeight(88)
        self.result = widgets.ResultCard("合成结果")
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
        self.btn_run = QtWidgets.QPushButton("开始合成")
        self.btn_run.setObjectName("Primary")
        widgets.label_button(self.btn_run, "合成", role="on_accent")
        self.btn_run.clicked.connect(self.run)
        self.btn_open = widgets.icon_button(
            "folder-open", "在文件管理器里打开输出目录")
        self.btn_open.setEnabled(False)
        self.btn_open.clicked.connect(self.open_out)
        bar.addWidget(self.btn_run)
        bar.addStretch(1)
        bar.addWidget(self.btn_open)
        side.addLayout(bar)

        pbox = widgets.Group("输出参数")
        side.addWidget(pbox)
        g = pbox.grid()
        g.setHorizontalSpacing(10)
        g.setVerticalSpacing(7)

        self.cb_fps = self._combo(FPS_CHOICES, editable=True, current="120")
        self.cb_aspect = self._combo([t for t, _ in ASPECT_CHOICES], index=0)
        self.cb_side = self._combo(SIDE_CHOICES, editable=True,
                                   current=SIDE_CHOICES[0])
        self.cb_codec = self._combo([t for t, _ in CODEC_CHOICES], index=0)

        crf_row = QtWidgets.QWidget()
        cl = QtWidgets.QHBoxLayout(crf_row)
        cl.setContentsMargins(0, 0, 0, 0)
        self.sl_crf = widgets.Slider()
        self.sl_crf.setRange(8, 35)
        self.sl_crf.setValue(18)
        self.lb_crf = QtWidgets.QLabel()
        self.lb_crf.setObjectName("ValueLabel")
        self.lb_crf.setMinimumWidth(110)
        cl.addWidget(self.sl_crf, 1)
        cl.addWidget(self.lb_crf)

        self.cb_preset = self._combo(PRESET_CHOICES, current="medium")
        self.sp_threads = QtWidgets.QSpinBox()
        self.sp_threads.setRange(0, 64)
        self.sp_threads.valueChanged.connect(self.refresh)

        self.lb_dur = QtWidgets.QLabel("")
        self.lb_dur.setObjectName("Hint")
        self.lb_dur.setWordWrap(True)

        for w, key in ((self.cb_fps, "合成/帧率"),
                       (self.cb_side, "合成/长边"),
                       (self.cb_aspect, "合成/比例"),
                       (self.cb_codec, "合成/编码器"),
                       (self.cb_preset, "合成/编码速度"),
                       (self.sp_threads, "合成/线程"),
                       (self.sl_crf, "合成/质量"),
                       (self.lb_crf, "合成/质量")):
            hints.attach(w, key)

        def drop(block, row, col, span=1):
            g.addWidget(widgets.field_block(*block), row, col, 1, span)

        drop(("帧率", self.cb_fps, hints.line("合成/帧率")), 0, 0)
        drop(("长边分辨率", self.cb_side, hints.line("合成/长边")), 0, 1)
        drop(("画面比例", self.cb_aspect, hints.line("合成/比例")), 1, 0, 2)
        drop(("编码器", self.cb_codec, hints.line("合成/编码器")), 2, 0, 2)
        drop(("编码速度", self.cb_preset,
              hints.line("合成/编码速度")), 3, 0)
        drop(("线程（0=自动）", self.sp_threads,
              hints.line("合成/线程")), 3, 1)
        drop(("质量 CRF（左=最清晰）", crf_row,
              hints.line("合成/质量")), 4, 0, 2)
        drop(("成片时长", self.lb_dur), 5, 0, 2)
        g.setColumnStretch(0, 1)
        g.setColumnStretch(1, 1)

        side.addWidget(self._build_output_box())
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

        # 路径手输要防抖：逐字符去扫目录会卡界面
        self._debounce = QtCore.QTimer(self)
        self._debounce.setSingleShot(True)
        self._debounce.setInterval(500)
        self._debounce.timeout.connect(self.detect)
        self.ed_dir.textChanged.connect(lambda _s: self._debounce.start())
        # textEdited 只在用户改时触发，setText 不会，省掉焦点判断
        self.ed_out.textEdited.connect(lambda _s: self._mark_manual())
        for w in (self.cb_fps, self.cb_side):
            w.editTextChanged.connect(lambda _s: self.refresh())
        for w in (self.cb_aspect, self.cb_codec, self.cb_preset):
            w.currentIndexChanged.connect(lambda _i: self.refresh())
        self.sl_crf.valueChanged.connect(self._on_crf)

        self._restore()
        self._on_crf(self.sl_crf.value())
        self.refresh()
        return page

    def _build_output_box(self):
        box = widgets.Group("输出文件")
        g = box.grid(vspacing=6)
        g.setHorizontalSpacing(10)
        g.setVerticalSpacing(6)
        self.ed_out = QtWidgets.QLineEdit()
        self.ed_out.setPlaceholderText("成片写到哪里（不含扩展名）")
        # 手输路径也要让"将写出"跟着变，不能只在另存为对话框里更新
        self.ed_out.textChanged.connect(lambda _s: self._refresh_labels())
        self.btn_save = widgets.icon_button(
            "download", "另存为…（选择成片路径）", box=30, size=17)
        self.btn_save.clicked.connect(self.choose_out)
        g.addWidget(self.ed_out, 0, 0)
        g.addWidget(self.btn_save, 0, 1)
        self.lb_out = QtWidgets.QLabel("")
        self.lb_out.setObjectName("FieldHint")
        self.lb_out.setWordWrap(True)
        self.lb_out.setSizePolicy(QtWidgets.QSizePolicy.Policy.Ignored,
                                  QtWidgets.QSizePolicy.Policy.Preferred)
        g.addWidget(self.lb_out, 1, 0, 1, 2)
        g.setColumnStretch(0, 1)
        return box

    def _combo(self, items, editable=False, current=None, index=None):
        c = QtWidgets.QComboBox()
        c.setEditable(editable)
        for it in items:
            if isinstance(it, tuple):
                c.addItem(it[0], it[1])
            else:
                c.addItem(it)
        if index is not None:
            c.setCurrentIndex(index)
        if current is not None:
            i = c.findData(current) if not editable else -1
            if i >= 0:
                c.setCurrentIndex(i)
            else:
                c.setCurrentText(current)
        c.setSizePolicy(QtWidgets.QSizePolicy.Policy.Ignored,
                        QtWidgets.QSizePolicy.Policy.Fixed)
        return c

    # ------------------------------------------------------------------ 取值
    def _aspect(self):
        return dict(ASPECT_CHOICES)[self.cb_aspect.currentText()]

    def _codec(self):
        return dict(CODEC_CHOICES)[self.cb_codec.currentText()]

    def _fps(self):
        m = re.search(r"\d+(\.\d+)?", self.cb_fps.currentText())
        return float(m.group()) if m else 0.0

    def _long_side(self):
        m = re.search(r"\d+", self.cb_side.currentText())
        return int(m.group()) if m else 0

    def _threads(self):
        return int(self.sp_threads.value())

    def _preset(self):
        return self.cb_preset.currentData() or ffmod.preset_value(
            self.cb_preset.currentText())

    def _on_crf(self, v):
        self.lb_crf.setText(f"{v}  {crf_word(v)}")
        self.refresh()

    def _mark_manual(self):
        self.out_manual = True
        self._refresh_labels()

    # ------------------------------------------------------------------ 检测
    def choose_dir(self):
        d = QtWidgets.QFileDialog.getExistingDirectory(
            self.shell, "选择延时照片序列目录")
        if d:
            self.ed_dir.setText(os.path.normpath(d))

    def detect(self):
        d = self.ed_dir.text().strip().strip('"')
        self.series = None
        if not d:
            self._set_info("（未选择目录）", False)
            self.preview.clear("选择序列目录后在这里预览首帧\n"
                               "也可以把整个文件夹拖进窗口")
            self.refresh()
            return
        if not os.path.isdir(d):
            self._set_info(f"目录不存在：{d}", False)
            self.preview.clear("目录不存在")
            self.refresh()
            return
        try:
            s = video.scan_series(d)
        except video.SeriesGapError as e:
            s = self._ask_gaps(d, e)
            if s is None:
                self._set_info("序列有缺口，已取消", False)
                self.preview.clear("序列有缺口\n补齐缺失帧后重试，或允许缺口继续")
                self.refresh()
                return
        except video.SeriesError as e:
            self._set_info(str(e), False)
            self.preview.clear("这个目录里没有可用的图片序列")
            self.refresh()
            return
        w, h = imio.image_size(os.path.join(d, s["sample"]))
        self.series = dict(s, dir=d, w=w, h=h)
        size = f"{w}x{h}" if w else "尺寸未知（未安装 cv2）"
        gap = f"   缺口 {len(s['gaps'])} 处（按上一帧停留）" if s.get("gaps") else ""
        self._set_info(
            f"序列  {s['prefix']}{s['first']:0{s['width']}d}.."
            f"{s['last']:0{s['width']}d}   共 {s['count']} 张   "
            f"{size}   源文件模式  {s['pattern']}{gap}", not s.get("gaps"))
        self.preview.set_frame(
            os.path.join(d, s["sample"]),
            f"首帧  {s['sample']}  {size}")
        self.refresh()

    def _ask_gaps(self, d, err):
        """缺帧不是错误：问用户要不要补位继续，选取消返回 None。"""
        box = QtWidgets.QMessageBox(self.shell)
        box.setWindowTitle("序列有缺口")
        box.setText(str(err))
        box.setIcon(QtWidgets.QMessageBox.Icon.Warning)
        retry = box.addButton("允许缺口，继续",
                              QtWidgets.QMessageBox.ButtonRole.AcceptRole)
        box.addButton("取消", QtWidgets.QMessageBox.ButtonRole.RejectRole)
        box.exec()
        if box.clickedButton() is not retry:
            return None
        return video.scan_series(d, allow_gaps=True)

    def _set_info(self, text, ok):
        self.lb_info.setText(text)
        self.lb_info.setObjectName("Info" if ok else "InfoBad")
        # objectName 变了要重新 polish 才会套用新 QSS
        st = self.lb_info.style()
        st.unpolish(self.lb_info)
        st.polish(self.lb_info)
        if not ok:
            self._stage = ""
        self.sync_page_hint()

    # ------------------------------------------------------------------ 刷新
    def refresh(self):
        self._refresh_out()
        self._refresh_labels()

    def _stem(self):
        s = self.series
        return os.path.splitext(video.default_out(
            s["dir"], s["prefix"], self._fps(), self._long_side()))[0]

    def _refresh_out(self):
        if not self.out_manual and self.series:
            self.ed_out.setText(self._stem())
        self._refresh_labels()

    def _jobs(self):
        """[(比例, 输出路径)]；两版时由 video.outputs_for 自动加后缀。"""
        stem = self.ed_out.text().strip().strip('"')
        if not stem:
            return []
        return list(video.outputs_for(self._aspect(), stem).items())

    def _dims(self, aspect):
        s = self.series or {}
        w, h = video.target_size(aspect, self._long_side(),
                                 s.get("w", 0), s.get("h", 0))
        return f"{w}x{h}" if h else f"{w}x?"

    def _refresh_labels(self):
        if not self.series:
            self.lb_dur.setText("")
            self.lb_out.setText("")
            self.lb_out.setToolTip("")
            return
        fps = self._fps()
        if fps > 0:
            sec = self.series["count"] / fps
            when = (f"{int(sec) // 60} 分 {int(sec) % 60} 秒" if sec >= 60
                    else f"{sec:.1f} 秒")
            self.lb_dur.setText(f"{when}   @ {fps:g}fps")
        else:
            self.lb_dur.setText("帧率必须是正数")
        jobs_ = self._jobs()
        if not jobs_:
            self.lb_out.setText("")
            self.lb_out.setToolTip("")
        else:
            # 标签里只放文件名，长路径塞进去会把整个检查器的宽度撑爆
            self.lb_out.setText("将写出：" + "、".join(
                f"{os.path.basename(p)}  ({self._dims(a)})"
                for a, p in jobs_))
            self.lb_out.setToolTip("将写出：\n" + "\n".join(
                f"{p}   ({self._dims(a)})" for a, p in jobs_))

    def choose_out(self):
        p, _ = QtWidgets.QFileDialog.getSaveFileName(
            self.shell, "输出文件（两版会自动加后缀）", "", "MP4 视频 (*.mp4)")
        if p:
            self.out_manual = True
            self.ed_out.setText(os.path.normpath(p))
            self._refresh_labels()

    # ------------------------------------------------------------------ 合成
    def run(self):
        ff = self.shell.ff
        if not ff:
            QtWidgets.QMessageBox.critical(
                self.shell, APP_TITLE,
                "未找到 ffmpeg，无法合成。\n请在运行本工具的 Python 环境里 "
                "pip install imageio-ffmpeg。")
            return
        if not self.series:
            self.warn("请先选择有效的图片序列目录。")
            return
        if self._fps() <= 0 or self._long_side() < 64:
            self.warn("帧率和长边分辨率必须是正数。")
            return
        todo = self._jobs()
        if not todo:
            self.warn("请填写输出文件路径。")
            return
        for _a, p in todo:
            od = os.path.dirname(p)
            if od:
                os.makedirs(od, exist_ok=True)

        o = {"fps": self._fps(), "long_side": self._long_side(),
             "crf": int(self.sl_crf.value()), "preset": self._preset(),
             "codec": self._codec(), "threads": self._threads()}
        s = dict(self.series)
        src_dir, pattern, start, count, cleanup = video.fill_gaps(s["dir"], s)
        if cleanup:
            self.log(f"缺口 {len(s['gaps'])} 处，已按上一帧补齐为 {count} 帧")
        self.log(f"参数: {o['codec']}  crf={o['crf']}（{crf_word(o['crf'])}）  "
                 f"preset={o['preset']}  长边={o['long_side']}  "
                 f"fps={o['fps']:g}  帧数={count}")
        if o["crf"] >= 28:
            self.log("提醒：CRF 越大画质越差，4K 成片一般用 17~20。")

        steps = []
        dims = []
        for a, out in todo:
            cmd, w, h = video.build_cmd(
                ff, src_dir, out, o["fps"], None if a == "none" else a,
                o["long_side"], o["crf"], o["preset"], o["codec"], pattern,
                start, count, s["w"], s["h"], o["threads"], False,
                progress=True)
            steps.append((f"出片 {w}×{h}", cmd, jobs.Meter("frame", count),
                          out, None))
            dims.append((out, f"{w}×{h}"))
        # 结果卡要说"这次跑是用什么参数跑的"，所以在这儿定格一份：
        # 跑完你可能马上改帧率，不能拿新值去解释刚出的成片
        self._run_info = {"fps": o["fps"], "frames": count,
                          "codec": o["codec"], "crf": o["crf"],
                          "gaps": len(s["gaps"]) if cleanup else 0,
                          "dims": dims}
        self._save()
        job = jobs.Job(steps)
        job.log.connect(self.log)
        if cleanup:
            job.finished.connect(cleanup)
        if self.shell.start_job(job, self._finished, self.result):
            self.btn_run.setEnabled(False)
            self.result.set_status("run", f"合成中 · {count} 帧 → {len(steps)} 段")
            self.result.set_metrics([])
            self.result.set_path(None)
            self.result.set_next("")
        elif cleanup:
            cleanup()

    def _finished(self, ok, res):
        self.btn_run.setEnabled(True)
        if ok and res:
            self.btn_open.setEnabled(True)
            self._last_out = res[0]
            self._stage = f"已合成 {len(res)} 段"
            self.sync_page_hint()
            self.log(f"\n全部完成：{len(res)} 个文件")
            self._show_result(res)
            self.status(f"合成完成 {len(res)} 段  {os.path.basename(res[0])}")
            self.shell.material.set_video(res[0])   # 流水线：交给裁剪/调色台
        elif res == "cancelled":
            self.log("已取消，未完成的文件已清除")
            self.result.set_status("idle", "已取消 · 未完成的文件已清除")
            self.result.set_next("参数还留着，直接再点「开始合成」。")
            self.status("已取消")
        else:
            self.log(f"失败：{res}")
            self.result.set_status("bad", "合成失败", res)
            self.result.set_next("展开技术细节看 ffmpeg 最后几行。")
            self.status("合成失败，详见日志")

    # 每个数字配一句"它是怎么来的、多大算正常"：成片看不懂的时候，
    # 用户其实不知道是该改帧率、改 CRF 还是回去重新对齐。
    METRICS = (
        ("成片时长", "帧数 ÷ 帧率。延时想要\"流畅又不太短\"，一般让成片落在 "
                    "10~30 秒；太长就提帧率，太短就降。"),
        ("分辨率", "每段的输出长宽。两版比例时这里会并列两个，"
                  "带裁切的那版边略少是比例取整造成的。"),
        ("帧率", "每秒放多少帧。源序列是 120fps 拍的，成片也用高帧率才顺滑；"
                "60fps 兼容性最好。"),
        ("画质", "编码 + CRF。CRF 越小越清晰、文件越大；4K 一般 17~20，"
                "超过 28 会明显糊。"),
        ("文件体积", "成片实际大小。同一 CRF 下画面越复杂越大；"
                    "体积异常小说明源帧可能是黑帧或没读全。"),
        ("补帧", "序列缺帧时按上一帧补齐的数量。非零就该回对齐页看看是不是漏拍；"
                "为 0 表示序列连续。"),
    )

    def _show_result(self, outs):
        info = getattr(self, "_run_info", None) or {}
        fps = info.get("fps") or 0
        n = info.get("frames") or 0
        sec = n / fps if fps else 0
        crf = info.get("crf")
        sizes = []
        for p in outs:
            try:
                sizes.append(os.path.getsize(p))
            except OSError:
                pass
        total = jobs.human_mb(sum(sizes)) if sizes else "—"
        dims = info.get("dims") or []
        vals = [
            (f"{int(sec) // 60} 分 {int(sec) % 60} 秒" if sec >= 60
             else f"{sec:.1f} 秒"),
            "、".join(d for _p, d in dims) if dims else "—",
            f"{fps:g} fps" if fps else "—",
            f"{info.get('codec', '')} CRF {crf}"
            + (f"（{crf_word(crf)}）" if crf is not None else "")
            if crf is not None else "—",
            total,
            f"{info.get('gaps', 0)} 帧",
        ]
        self.result.set_metrics(
            [(name, val, tip) for (name, tip), val in zip(self.METRICS, vals)])
        small = any(s < 100_000 for s in sizes)
        self.result.set_status(
            "warn" if small else "ok",
            f"合成完成 · {len(outs)} 段 · {n} 帧 → {total}",
            "有成品文件不到 100KB，可能源帧没读全或画面全黑" if small else None)
        label = "成片：" if len(outs) == 1 else f"成片 {len(outs)} 段："
        self.result.set_path(outs[0], label)
        self.result.set_next(
            "先看一眼成片，再去配乐模块加音乐；要无损交付就切到裁剪模块改边。"
            if not small else "重新检查序列目录，必要时回对齐页重跑。")

    def open_out(self):
        widgets.reveal(self._last_out)

    # ------------------------------------------------------------------ 流水线
    def on_material(self, kind, path):
        if kind == "series" and path and \
                os.path.normpath(path) != os.path.normpath(self.ed_dir.text()):
            self.ed_dir.setText(os.path.normpath(path))   # textChanged -> 防抖检测

    def log(self, text):
        self.log_view.appendPlainText(text)
        sb = self.log_view.verticalScrollBar()
        sb.setValue(sb.maximum())

    # ------------------------------------------------------------------ 记忆
    def _restore(self):
        s = self.settings()
        for key, cb in (("fps", self.cb_fps), ("side", self.cb_side),
                        ("aspect", self.cb_aspect), ("codec", self.cb_codec),
                        ("preset", self.cb_preset)):
            if s.contains(f"{self.name}/{key}"):
                v = s.value(f"{self.name}/{key}", "")
                i = cb.findData(v)
                if i >= 0:
                    cb.setCurrentIndex(i)
                else:
                    cb.setCurrentText(v)
        if s.contains(f"{self.name}/crf"):
            self.sl_crf.setValue(int(s.value(f"{self.name}/crf", 18)))
        if s.contains(f"{self.name}/threads"):
            self.sp_threads.setValue(int(s.value(f"{self.name}/threads", 0)))
        if s.contains(f"{self.name}/dir"):
            self.ed_dir.setText(s.value(f"{self.name}/dir", ""))

    def _save(self):
        s = self.settings()
        s.setValue(f"{self.name}/dir", self.ed_dir.text())
        s.setValue(f"{self.name}/fps", self.cb_fps.currentText())
        s.setValue(f"{self.name}/aspect", self.cb_aspect.currentText())
        s.setValue(f"{self.name}/side", self.cb_side.currentText())
        s.setValue(f"{self.name}/codec", self.cb_codec.currentText())
        s.setValue(f"{self.name}/crf", self.sl_crf.value())
        s.setValue(f"{self.name}/preset", self._preset())
        s.setValue(f"{self.name}/threads", self.sp_threads.value())

    def shutdown(self):
        self._save()
