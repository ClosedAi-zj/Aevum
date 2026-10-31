# -*- coding: utf-8 -*-
"""对齐模块：估计每帧位移，统一裁切，输出对齐后的序列。

apps/align.py（命令行）的 Qt 化。算法全在 acceptance/reference/align.py；这里把两步流程
交给 AlignJob 在后台跑，进度与取消走外壳那条全局进度条。
写完通过 Material.set_series 交给合成模块，形成 对齐 → 合成 → 裁剪 → 调色台 的完整链。
"""
import glob
import os
import shutil
import time

import numpy as np
from PySide6 import QtCore, QtWidgets

from reference import align, imio
from reference.gui import hints
from reference.gui import job as jobs
from reference.gui import widgets
from reference.gui.module import Module

EST_CHOICES = [("模板匹配（对垂直漂移最稳）", "template"),
               ("相位相关（整体大位移时用）", "phase"),
               ("ECC 平移+旋转（相机被碰歪）", "ecc:euclid"),
               ("ECC 单应（透视/形变）", "ecc:homography"),
               ("特征点（大位移/遮挡/带旋转）", "feature")]
OUT_CHOICES = [("JPG（体积小，日常够用）", "jpg", 8),
               ("PNG 16bit（像素无损）", "png", 16),
               ("TIFF 16bit（像素无损）", "tif", 16)]


class _Cancelled(Exception):
    pass


class AlignJob(jobs.Job):
    """两阶段后台任务：测位移（可只做采样预检）→ 平滑/裁切 → 写出。

    不跑 ffmpeg，但沿用 jobs.Job 的进度信号与取消按钮。
    取消时把本次写出的图删干净；预检模式不落盘，取消即止。
    """

    def __init__(self, files, ref_index, opts, out_dir, sample_only=False):
        super().__init__([])
        self.files = files
        self.ref_index = ref_index
        self.opts = opts
        self.out_dir = out_dir
        self.sample_only = sample_only
        self.summary = {}               # 预检/全量结果，界面右侧卡片直接读
        self.t0 = time.time()
        self._dir_fresh = out_dir and not os.path.isdir(out_dir)
        # 结果卡的时间线：预检只测位移，全量还要裁切、写盘，校验是可选的
        self.timeline = (["测位移"] if sample_only else
                         ["测位移", "平滑与裁切", "写出序列"]
                         + (["抽帧校验"] if opts["verify"] else []))
        self._ph = 0

    # ---- 进度回调在 Job 线程里被 align.* 调用；取消就抛出去，让 Pool 收摊 ----
    def _tick(self, span, base, label):
        def cb(done, total):
            if self.stopping:
                raise _Cancelled
            self.progress.emit(base + span * done / max(total, 1),
                               f"{label} {done}/{total}")
        return cb

    def run(self):
        try:
            self._run()
        except _Cancelled:
            self.stage.emit(self._ph, "bad", "已取消")
            self._cleanup()
            self.done.emit(False, "cancelled")
        except Exception as e:                       # 读图失败之类，如实报告
            self.stage.emit(self._ph, "bad", str(e)[:24])
            self._cleanup()
            self.log.emit(f"对齐失败：{e}")
            self.done.emit(False, str(e))

    def _run(self):
        o = self.opts
        n = len(self.files)
        est_label = o["est"] + (f"/{o['motion']}" if o["est"] == "ecc" else "")
        self.log.emit(f"{n} 张  估计器 {est_label}  参考 "
                      f"{os.path.basename(self.files[self.ref_index])}")
        mats = {} if o["est"] in ("ecc", "feature") else None
        self.stage.emit(0, "run", "")
        offsets, scores, issues, dt = align.measure_offsets(
            self.files, self.ref_index, est=o["est"], workers=o["workers"],
            template_start=o["template_start"], search_margin=o["search_margin"],
            motion=o["motion"], half=o["half"], mats=mats,
            on_progress=self._tick(0.5, 0.0, "测位移"))
        self.stage.emit(0, "ok", f"{dt:.1f}s 异常 {len(issues)} 帧")
        self.log.emit(f"位移估计 {dt:.1f}s  异常 {len(issues)} 帧")
        for i, msg in issues[:12]:
            self.log.emit(f"  [{i:4d}] {msg}")

        xs = [offsets[i][0] for i in range(n)]
        ys = [offsets[i][1] for i in range(n)]
        self.log.emit(f"dx 范围 [{min(xs):+.2f}, {max(xs):+.2f}]  "
                      f"dy 范围 [{min(ys):+.2f}, {max(ys):+.2f}]")
        rot = None
        if mats is not None:
            rots = [align.rotation_deg(mats[i]) for i in range(n)]
            rot = (min(rots), max(rots))
            self.log.emit(f"旋转范围 [{rot[0]:+.2f}°, {rot[1]:+.2f}°]")
        low = [(i, round(scores[i], 3)) for i in range(n)
               if scores[i] < o["min_score"]]
        if low:
            self.log.emit(f"置信度偏低（可能误匹配，共 {len(low)} 帧）："
                          f"{low[:10]}")
        if self.sample_only:
            x1, y1, x2, y2 = align.common_crop(
                mats if mats is not None else offsets, o["w"], o["h"])
            loss = 100.0 * (1 - (x2 - x1) * (y2 - y1) / float(o["w"] * o["h"]))
            self.summary = {"n": n, "dx": (min(xs), max(xs)),
                            "dy": (min(ys), max(ys)), "crop": (x1, y1, x2, y2),
                            "loss": loss, "issues": len(issues), "out": None,
                            "rot": rot}
            self.log.emit(f"按采样粗算：裁切 ({x1},{y1})-({x2},{y2})  "
                          f"面积损失约 {loss:.1f}%（全量结果以此为准）")
            self.done.emit(True, None)
            return
        if self.stopping:
            raise _Cancelled

        self._ph = 1
        self.stage.emit(1, "run", "")
        if o["smooth"] > 1:
            offsets, kx, ky = align.smooth_offsets(
                offsets, scores, o["smooth"], o["jump_px"], o["min_score"])
            if mats is not None:
                mats = align.retranslate(mats, offsets)
            self.log.emit(f"median 平滑（窗口 {o['smooth']}）：保留真实抖动 "
                          f"dx {kx} 帧 / dy {ky} 帧")
        x1, y1, x2, y2 = align.common_crop(
            mats if mats is not None else offsets, o["w"], o["h"])
        cw, ch = x2 - x1, y2 - y1
        loss = 100.0 * (1 - cw * ch / float(o["w"] * o["h"]))
        self.summary = {"n": n, "dx": (min(xs), max(xs)),
                        "dy": (min(ys), max(ys)), "crop": (x1, y1, x2, y2),
                        "loss": loss, "issues": len(issues), "out": (cw, ch),
                        "rot": rot}
        self.log.emit(f"统一裁切 ({x1},{y1})-({x2},{y2})  输出 {cw}x{ch}  "
                      f"面积损失 {loss:.2f}%")
        self.stage.emit(1, "ok", f"裁到 {cw}x{ch}")
        if cw <= 100 or ch <= 100:
            raise RuntimeError("有效区域太小，先查参考帧或参数")

        self.log.emit(f"写出对齐序列 → {self.out_dir}")
        self._ph = 2
        self.stage.emit(2, "run", "")
        out, failed = align.apply_offsets(
            self.files, self.out_dir, offsets, (x1, y1, x2, y2),
            (o["h"], o["w"]), out_ext=o["out_ext"], bps=o["bps"],
            half=o["half"], workers=o["workers"], mats=mats,
            on_progress=self._tick(0.45, 0.55, "写图"))
        self.stage.emit(2, "ok", f"{len(out)} 张")
        if failed:
            self.log.emit(f"写入失败 {len(failed)} 帧：{failed[:10]}")

        if o["verify"]:
            self._ph = 3
            self.stage.emit(3, "run", "")
            check = list(range(len(out)))
            if len(check) > 24:
                check = sorted(set(np.linspace(0, len(out) - 1, 12).astype(int))
                               | {self.ref_index})
            v = align.verify_residuals([out[i] for i in check],
                                       ref_index=check.index(self.ref_index))
            mx = 0.0
            for _i, s, _r in v:
                if s is not None:
                    mx = max(mx, float(np.hypot(*s)))
            self.stage.emit(3, "ok", f"最大残余 {mx:.2f}px")
            self.log.emit(f"抽 {len(check)} 帧校验：最大残余 {mx:.2f}px"
                          + ("（理想 < 1px）" if mx < 1.0 else "，超过 1px，建议查参数"))
        self.progress.emit(1.0, "完成")
        self.done.emit(True, self.out_dir)

    def _cleanup(self):
        """取消/失败不留半成品：新目录整锅端走，老目录只删本次写的。"""
        if self.sample_only or not self.out_dir:
            return
        if self._dir_fresh and os.path.isdir(self.out_dir):
            shutil.rmtree(self.out_dir, ignore_errors=True)
            self.log.emit(f"已删除输出目录 {self.out_dir}")
            return
        gone = 0
        for p in glob.glob(os.path.join(self.out_dir, "aligned_*")):
            try:
                if os.path.getmtime(p) >= self.t0 - 1:
                    os.remove(p)
                    gone += 1
            except OSError:
                pass
        if gone:
            self.log.emit(f"已删除本次写出的 {gone} 张")


class Aligner(Module):
    name = "对齐"

    def __init__(self, shell):
        super().__init__(shell)
        self.files = None
        self.is_raw = False
        self.out_manual = False
        self._own_out = None
        self._stage = ""

    def top_hint(self):
        """顶栏只报状态：多少张 + 走到哪一步，教程文案在参数下面那行。"""
        if not self.files:
            return ""
        return f"{len(self.files)} 张 · {self._stage}" if self._stage \
            else f"{len(self.files)} 张"

    # ------------------------------------------------------------------ UI
    def build(self):
        page = QtWidgets.QWidget()
        v = QtWidgets.QVBoxLayout(page)
        v.setContentsMargins(14, 12, 14, 12)
        v.setSpacing(10)

        top = QtWidgets.QHBoxLayout()
        top.setSpacing(8)
        self.btn_dir = widgets.icon_button(
            "folder-open", "选择序列目录（Ctrl+Shift+O）")
        self.btn_dir.clicked.connect(self.choose_dir)
        self.ed_dir = QtWidgets.QLineEdit()
        self.ed_dir.setPlaceholderText("照片序列目录；也可以把文件夹直接拖进窗口")
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
            empty="选择照片目录后在这里预览参考帧\n也可以把整个文件夹拖进窗口")
        left.addWidget(self.preview, 3)
        self.log_view = QtWidgets.QPlainTextEdit()
        self.log_view.setObjectName("Log")
        self.log_view.setReadOnly(True)
        self.log_view.setPlaceholderText(
            "逐帧位移、统一裁切与异常帧清单写在这里。")
        self.log_view.setLineWrapMode(
            QtWidgets.QPlainTextEdit.LineWrapMode.NoWrap)
        self.log_view.setMinimumHeight(88)
        self.result = widgets.ResultCard("对齐结果")
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
        self.btn_probe = widgets.icon_button(
            "zoom-in", "预检 · 只抽几帧测位移，不写任何文件，几秒出结果")
        self.btn_probe.clicked.connect(self.probe)
        self.btn_run = QtWidgets.QPushButton("开始对齐")
        self.btn_run.setObjectName("Primary")
        widgets.label_button(self.btn_run, "对齐", role="on_accent")
        self.btn_run.clicked.connect(self.run)
        self.btn_open = widgets.icon_button(
            "folder-open", "在文件管理器里打开输出目录")
        self.btn_open.setEnabled(False)
        self.btn_open.clicked.connect(self.open_out)
        bar.addWidget(self.btn_probe)
        bar.addWidget(self.btn_run)
        bar.addStretch(1)
        bar.addWidget(self.btn_open)
        side.addLayout(bar)

        pbox = widgets.Group("处理参数")
        side.addWidget(pbox)
        g = pbox.grid()
        g.setHorizontalSpacing(10)
        g.setVerticalSpacing(7)

        self.cb_est = QtWidgets.QComboBox()
        self.cb_est.addItems([t for t, _ in EST_CHOICES])
        self.cb_est.setSizePolicy(QtWidgets.QSizePolicy.Policy.Ignored,
                                  QtWidgets.QSizePolicy.Policy.Fixed)
        self.sp_tstart = QtWidgets.QDoubleSpinBox()
        self.sp_tstart.setRange(0.3, 0.9)
        self.sp_tstart.setSingleStep(0.05)
        self.sp_tstart.setValue(0.7)
        self.sp_margin = QtWidgets.QSpinBox()
        self.sp_margin.setRange(0, 500)
        self.sp_margin.setValue(150)
        self.sp_smooth = QtWidgets.QSpinBox()
        self.sp_smooth.setRange(1, 15)
        self.sp_smooth.setValue(3)
        self.sp_jump = QtWidgets.QDoubleSpinBox()
        self.sp_jump.setRange(0.2, 5.0)
        self.sp_jump.setSingleStep(0.1)
        self.sp_jump.setValue(1.0)
        self.sp_score = QtWidgets.QDoubleSpinBox()
        self.sp_score.setRange(0.30, 0.95)
        self.sp_score.setSingleStep(0.05)
        self.sp_score.setValue(0.6)
        self.sp_sample = QtWidgets.QSpinBox()
        self.sp_sample.setRange(3, 200)
        self.sp_sample.setValue(12)
        self.chk_raw = QtWidgets.QCheckBox("输入是 RAW（.RW2 / .CR3…）")
        self.chk_raw.toggled.connect(self._raw_toggled)
        self.chk_half = QtWidgets.QCheckBox("RAW 半分辨率解码（快约 4 倍，精度略降）")
        self.chk_half.setEnabled(False)
        self.chk_verify = QtWidgets.QCheckBox("完成后抽帧校验残余位移")
        self.chk_verify.setChecked(True)
        for w, key in ((self.cb_est, "对齐/估计器"),
                       (self.sp_tstart, "对齐/模板起始"),
                       (self.sp_margin, "对齐/搜索余量"),
                       (self.sp_smooth, "对齐/平滑窗口"),
                       (self.sp_jump, "对齐/抖动阈值"),
                       (self.sp_score, "对齐/置信度"),
                       (self.sp_sample, "对齐/抽帧"),
                       (self.chk_raw, "对齐/RAW"),
                       (self.chk_half, "对齐/半分辨率"),
                       (self.chk_verify, "对齐/校验")):
            hints.attach(w, key)

        def drop(block, row, col, span=1):
            g.addWidget(widgets.field_block(*block), row, col, 1, span)

        drop(("位移估计器", self.cb_est, hints.line("对齐/估计器")), 0, 0, 2)
        drop(("模板起始高度", self.sp_tstart,
              hints.line("对齐/模板起始")), 1, 0)
        drop(("搜索余量（px）", self.sp_margin,
              hints.line("对齐/搜索余量")), 1, 1)
        drop(("平滑窗口", self.sp_smooth, hints.line("对齐/平滑窗口")), 2, 0)
        drop(("真实抖动阈值（px）", self.sp_jump,
              hints.line("对齐/抖动阈值")), 2, 1)
        drop(("置信度门槛", self.sp_score, hints.line("对齐/置信度")), 3, 0)
        drop(("预检抽帧", self.sp_sample, hints.line("对齐/抽帧")), 3, 1)
        g.addWidget(self.chk_raw, 4, 0, 1, 2)
        g.addWidget(self.chk_half, 5, 0, 1, 2)
        g.addWidget(self.chk_verify, 6, 0, 1, 2)
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

        # 手输路径防抖，跟合成模块同一套
        self._debounce = QtCore.QTimer(self)
        self._debounce.setSingleShot(True)
        self._debounce.setInterval(500)
        self._debounce.timeout.connect(self.detect)
        self.ed_dir.textChanged.connect(lambda _s: self._debounce.start())
        self.cb_est.currentIndexChanged.connect(self._est_changed)
        self._est_changed()
        self._restore()
        self._refresh_out()
        return page

    def _build_output_box(self):
        box = widgets.Group("输出")
        g = box.grid(vspacing=6)
        g.setHorizontalSpacing(10)
        g.setVerticalSpacing(6)
        lb = QtWidgets.QLabel("输出目录")
        lb.setObjectName("FieldLabel")
        g.addWidget(lb, 0, 0, 1, 2)
        self.ed_out = QtWidgets.QLineEdit()
        self.ed_out.setPlaceholderText("对齐后的序列目录")
        self.ed_out.textEdited.connect(lambda _s: self._mark_manual())
        self.btn_out = widgets.icon_button(
            "folder-open", "浏览输出目录", box=30, size=17)
        self.btn_out.clicked.connect(self.choose_out)
        g.addWidget(self.ed_out, 1, 0)
        g.addWidget(self.btn_out, 1, 1)
        self.cb_out = QtWidgets.QComboBox()
        self.cb_out.addItems([t for t, _, _ in OUT_CHOICES])
        self.cb_out.setSizePolicy(QtWidgets.QSizePolicy.Policy.Ignored,
                                  QtWidgets.QSizePolicy.Policy.Fixed)
        hints.attach(self.cb_out, "对齐/输出格式")
        g.addWidget(widgets.field_block(
            "格式", self.cb_out, hints.line("对齐/输出格式")), 2, 0, 1, 2)
        hint = QtWidgets.QLabel(
            "默认在输入目录旁生成 *_aligned；不存在的目录会在完成时新建")
        hint.setObjectName("FieldHint")
        hint.setWordWrap(True)
        g.addWidget(hint, 3, 0, 1, 2)
        g.setColumnStretch(0, 1)
        return box

    # 每个指标都带一句"这个数字说明什么、多大算不正常"：
    # 看不懂输出比看不懂参数更让人不敢走下一步。
    METRICS = (
        ("帧数", "参与这次计算的帧数。预检只跑抽样的几帧，全量才是整个序列。"),
        ("水平位移", "相邻帧左右移动的范围。一直朝一个方向偏是机位在走或摇镜，"
                    "来回摆才是抖动；范围越大，最后裁掉的边越多。"),
        ("垂直位移", "上下方向的同一件事。持续向下漂移多半是三脚架滑或风，"
                    "对称的小幅摆动是正常持机抖动。"),
        ("旋转", "帧间旋转角度范围，只有 ECC/特征点估计器会给。"
                "超过 ±0.3° 值得抽两张相邻帧对一下边线。"),
        ("统一裁切", "所有帧都留下的公共区域，也就是输出画面的来源框，"
                    "坐标是左上/右下像素。"),
        ("面积损失", "裁切丢掉的画面百分比。5% 以内放心，5~15% 常见，"
                    "超过 20% 说明某几帧位移太大，先查异常帧或换参考帧。"),
        ("输出尺寸", "对齐序列的单张分辨率。太小会在合成时被放大、发糊。"),
        ("异常帧", "测不出可靠位移的帧数，这些帧沿用上一帧的变换。"
                  "非零不等于失败，展开技术细节能看到是哪几帧、为什么。"),
    )

    def _show_summary(self, s, mode="full"):
        """把 summary 摊到结果卡上：结论在最上、数字成排，解释和下一步进悬停。"""
        if not s:
            self.result.set_status("idle", "对齐结果")
            self.result.set_metrics([])
            self.result.set_path(None)
            return
        n = s.get("n") or 0
        rot = s.get("rot")
        x1, y1, x2, y2 = s["crop"]
        vals = [f"{n}",
                f"{s['dx'][0]:+.1f} ~ {s['dx'][1]:+.1f} px",
                f"{s['dy'][0]:+.1f} ~ {s['dy'][1]:+.1f} px",
                "—" if not rot else f"{rot[0]:+.2f}° ~ {rot[1]:+.2f}°",
                f"({x1},{y1}) – ({x2},{y2})",
                f"{s['loss']:.1f}%",
                "—" if not s["out"] else f"{s['out'][0]}×{s['out'][1]}",
                f"{s['issues']}"]
        self.result.set_metrics(
            [(name, val, tip) for (name, tip), val in zip(self.METRICS, vals)])
        # 只有"损失过大 / 异常帧成规模"才标黄：异常帧非零本身不是失败
        bad = s["loss"] > 20 or s["issues"] > max(2, n * 0.02)
        if mode == "probe":
            self.result.set_status(
                "ok", f"预检完成 · 抽样 {n} 帧，未写文件",
                "预检只测位移不产出序列，裁切与损失按抽样粗算，跑全量才定稿")
            self.result.set_path(None)
            self.result.set_next(
                f"面积损失约 {s['loss']:.1f}%，偏大：先换参考帧或查抖动来源"
                if bad else "范围合理就跑「开始对齐」。")
        else:
            out = s["out"]
            self.result.set_status(
                "warn" if bad else "ok",
                f"对齐完成 · {n} 帧 → {out[0]}×{out[1]}" if out
                else f"对齐完成 · {n} 帧",
                "裁切偏大或有异常帧，展开技术细节确认是哪几帧" if bad else None)
            self.result.set_next(
                "先看技术细节里的异常帧清单，再决定要不要重跑"
                if bad else "序列已交给合成模块，切过去直接出片。")

    def _est_changed(self):
        tpl = dict(EST_CHOICES)[self.cb_est.currentText()] == "template"
        for w in (self.sp_tstart, self.sp_margin):
            w.setEnabled(tpl)

    def _raw_toggled(self, on):
        self.chk_half.setEnabled(on)
        if not on:
            self.chk_half.setChecked(False)
        self.detect()

    # ------------------------------------------------------------------ 检测
    def choose_dir(self):
        d = QtWidgets.QFileDialog.getExistingDirectory(
            self.shell, "选择延时照片目录")
        if d:
            self.out_manual = False
            self.ed_dir.setText(os.path.normpath(d))

    def detect(self):
        d = self.ed_dir.text().strip().strip('"')
        self.files = None
        self._stage = ""
        if d != getattr(self, "_sum_dir", None):
            self._sum_dir = d
            self._show_summary({})
        if not d:
            self._set_info("（未选择目录）", False)
            self.preview.clear("选择照片目录后在这里预览参考帧\n"
                               "也可以把整个文件夹拖进窗口")
            return
        if not os.path.isdir(d):
            self._set_info(f"目录不存在：{d}", False)
            self.preview.clear("目录不存在")
            return
        self.is_raw = self.chk_raw.isChecked()
        files = imio.scan_files(d, raw=self.is_raw)
        if not files and not self.is_raw:
            files = imio.scan_files(d, raw=True)      # 没有常规图就试 RAW
            if files:
                self.is_raw = True
                self.chk_raw.blockSignals(True)
                self.chk_raw.setChecked(True)
                self.chk_raw.blockSignals(False)
                self.chk_half.setEnabled(True)
        if not files:
            self._set_info(f"目录里没有可处理的图片：{d}", False)
            self.preview.clear("这个目录里没有可对齐的图片")
            return
        w, h = imio.image_size(files[0])
        self.files = files
        kind = "RAW" if self.is_raw else os.path.splitext(files[0])[1].lstrip(".").upper()
        self._set_info(f"{len(files)} 张  {kind}  {w}x{h}  "
                       f"{os.path.basename(files[0])}…", True)
        self.preview.set_frame(
            files[0], f"参考帧  {os.path.basename(files[0])}  {w}×{h}")
        self._refresh_out()

    def _set_info(self, text, ok):
        self.lb_info.setText(text)
        self.lb_info.setObjectName("Info" if ok else "InfoBad")
        st = self.lb_info.style()
        st.unpolish(self.lb_info)
        st.polish(self.lb_info)
        if not ok:
            self._stage = ""
        self.sync_page_hint()

    def _mark_manual(self):
        self.out_manual = True

    def _refresh_out(self):
        if self.out_manual or not self.files:
            return
        d = self.ed_dir.text().strip().strip('"')
        if d:
            self.ed_out.setText(os.path.join(
                os.path.dirname(d.rstrip("\\/")),
                os.path.basename(d.rstrip("\\/")) + "_aligned"))

    def choose_out(self):
        d = QtWidgets.QFileDialog.getExistingDirectory(
            self.shell, "选择输出目录（不存在的会在完成时新建）")
        if d:
            self.out_manual = True
            self.ed_out.setText(os.path.normpath(d))

    # ------------------------------------------------------------------ 跑
    def _opts(self):
        _t, out_ext, bps = OUT_CHOICES[self.cb_out.currentIndex()]
        est = dict(EST_CHOICES)[self.cb_est.currentText()]
        est, _, motion = est.partition(":")
        return {"est": est, "motion": motion or "euclid",
                "template_start": float(self.sp_tstart.value()),
                "search_margin": int(self.sp_margin.value()),
                "smooth": int(self.sp_smooth.value()),
                "jump_px": float(self.sp_jump.value()),
                "min_score": float(self.sp_score.value()),
                "workers": 0,           # 0 = 按 CPU 核数自动
                "half": self.chk_half.isChecked() and self.is_raw,
                "verify": self.chk_verify.isChecked(),
                "out_ext": out_ext, "bps": bps,
                "w": self._w, "h": self._h}

    def _prepare(self):
        """公共前置：探测尺寸 + 决定参与测量的文件子集与参考帧。"""
        if not self.files:
            self.warn("请先选择有效的照片目录。")
            return None
        w, h = imio.image_size(self.files[0])
        if not w:
            self.warn("读不出参考图尺寸。")
            return None
        self._w, self._h = w, h
        self._save()
        return self._opts()

    def _start(self, job, done):
        job.log.connect(self.log)
        self._job = job
        if self.shell.start_job(job, done, self.result):
            self.btn_run.setEnabled(False)
            self.btn_probe.setEnabled(False)
            return True
        return False

    def _running(self, text):
        """跑起来就把卡片切到"进行中"，日志自动摊开：进度条只有百分比，
        日志才说得出当前在测位移还是在写图。"""
        self.result.set_status("run", text)
        self.result.set_metrics([])
        self.result.set_path(None)
        self.result.set_next("")

    def probe(self):
        o = self._prepare()
        if not o:
            return
        k = int(self.sp_sample.value())
        if 0 < k < len(self.files):
            idx = sorted(set(align.sample_indices(len(self.files), k))
                         | {0})
            files = [self.files[i] for i in idx]
        else:
            files = list(self.files)
        self.log(f"预检：抽 {len(files)} 帧（不写文件）")
        job = AlignJob(files, 0, dict(o, verify=False, smooth=1),
                       None, sample_only=True)
        if self._start(job, self._probe_done):
            self._running(f"预检中 · 抽 {len(files)} 帧测位移")

    def _probe_done(self, ok, res):
        self.btn_run.setEnabled(True)
        self.btn_probe.setEnabled(True)
        if res == "cancelled":
            self.log("预检已取消")
            self.result.set_status("idle", "预检已取消")
        elif not ok:
            self.result.set_status("bad", "预检失败", res)
            self.result.set_next("展开技术细节看最后几行。")
            self.status("预检失败，详见日志")
        else:
            self._show_summary(self._job.summary, "probe")
            self._stage = "已预检"
            self.sync_page_hint()
            self.status("预检完成，看技术细节里的位移范围再决定是否跑全量")

    def run(self):
        o = self._prepare()
        if not o:
            return
        out = self.ed_out.text().strip().strip('"')
        if not out:
            self.warn("请填写输出目录。")
            return
        if os.path.normpath(out) == os.path.normpath(
                self.ed_dir.text().strip().strip('"')):
            self.warn("输出目录不能和输入目录相同。")
            return
        self.log(f"全量对齐 {len(self.files)} 张 → {out}")
        self._own_out = os.path.normpath(out)
        job = AlignJob(list(self.files), 0, o, out)
        if self._start(job, self._finished):
            self._running(f"对齐中 · {len(self.files)} 帧")

    def _finished(self, ok, res):
        self.btn_run.setEnabled(True)
        self.btn_probe.setEnabled(True)
        if ok and res:
            self.btn_open.setEnabled(True)
            self._last_out = res
            self._show_summary(self._job.summary)
            self.result.set_path(res, "成品序列：")
            self.log("\n完成")
            self._save()
            self._stage = "已对齐"
            self.sync_page_hint()
            self.status(f"对齐完成  {os.path.basename(res)}，可切到合成模块")
            self.shell.material.set_series(res)     # 流水线：交给合成
        elif res == "cancelled":
            self.log("已取消，本次写出的文件已清除")
            self.result.set_status("idle", "已取消 · 本次写出的文件已清除")
            self.result.set_next("参数还留着，直接再点「开始对齐」。")
            self.status("已取消")
        else:
            self.log(f"失败：{res}")
            self.result.set_status("bad", "对齐失败", res)
            self.result.set_next("展开技术细节看失败前最后几行。")
            self.status("对齐失败，详见日志")

    def open_out(self):
        widgets.reveal(getattr(self, "_last_out", None))

    # ------------------------------------------------------------------ 流水线
    def on_material(self, kind, path):
        if kind != "series" or not path:
            return
        if self._own_out and os.path.normpath(path) == self._own_out:
            return                      # 自己刚写的序列，别回灌当新输入
        if os.path.normpath(path) != os.path.normpath(self.ed_dir.text()):
            self.out_manual = False
            self.ed_dir.setText(os.path.normpath(path))   # textChanged -> 防抖检测

    def log(self, text):
        self.log_view.appendPlainText(text)
        sb = self.log_view.verticalScrollBar()
        sb.setValue(sb.maximum())

    # ------------------------------------------------------------------ 记忆
    def _restore(self):
        s = self.settings()
        if s.contains(f"{self.name}/dir"):
            self.ed_dir.setText(s.value(f"{self.name}/dir", ""))
            self.detect()
        for key, w in (("tstart", self.sp_tstart), ("margin", self.sp_margin),
                       ("smooth", self.sp_smooth), ("jump", self.sp_jump),
                       ("score", self.sp_score), ("sample", self.sp_sample)):
            if s.contains(f"{self.name}/{key}"):
                w.setValue(float(s.value(f"{self.name}/{key}")))
        if s.contains(f"{self.name}/est"):
            self.cb_est.setCurrentText(s.value(f"{self.name}/est", ""))
        if s.contains(f"{self.name}/outfmt"):
            self.cb_out.setCurrentText(s.value(f"{self.name}/outfmt", ""))
        self.chk_verify.setChecked(
            s.value(f"{self.name}/verify", True, type=bool))

    def _save(self):
        s = self.settings()
        s.setValue(f"{self.name}/dir", self.ed_dir.text())
        s.setValue(f"{self.name}/est", self.cb_est.currentText())
        s.setValue(f"{self.name}/tstart", self.sp_tstart.value())
        s.setValue(f"{self.name}/margin", self.sp_margin.value())
        s.setValue(f"{self.name}/smooth", self.sp_smooth.value())
        s.setValue(f"{self.name}/jump", self.sp_jump.value())
        s.setValue(f"{self.name}/score", self.sp_score.value())
        s.setValue(f"{self.name}/sample", self.sp_sample.value())
        s.setValue(f"{self.name}/outfmt", self.cb_out.currentText())
        s.setValue(f"{self.name}/verify", self.chk_verify.isChecked())

    def shutdown(self):
        self._save()
