# -*- coding: utf-8 -*-
"""后台任务共享层：跑 ffmpeg、解析进度、取消、删半成品。

合成、裁剪、调色台导出以前各抄了一份轮询和取消，行为还不一致（裁剪取消了
会留下不可播放的半成品）。这里统一成一个 QThread 子类：线程只发信号，控件
只在主线程动，跨线程读控件的老坑不会出现。
"""
import os
import re
import subprocess

from PySide6 import QtCore

from lapse.ff import NO_WINDOW

_RE_FRAME = re.compile(r"frame=\s*(\d+)")
_RE_TIME_US = re.compile(r"out_time_us=(\d+)")
_RE_SPEED = re.compile(r"speed=\s*([0-9.]+)x")
_RE_SIZE = re.compile(r"total_size=(\d+)")


def human_mb(n):
    return f"{n / 1e6:.1f}MB"


class Meter:
    """把 ffmpeg `-progress` 的行翻译成 0~1 分数。

    kind="frame" 按帧数（图片序列合成，总数确切已知）；
    kind="time"  按时长（视频裁剪/转码，只有 out_time_us）。
    """

    def __init__(self, kind, total=0.0):
        if kind not in ("frame", "time"):
            raise ValueError(f"未知进度类型: {kind}")
        self.kind = kind
        self.total = float(total) if kind == "time" else max(float(total), 1.0)
        self.speed = ""
        self.size = 0
        self.t = 0.0

    def feed(self, line):
        """返回 (frac, detail)；不是进度行时 frac 为 None，交上层当日志。"""
        if line.startswith("frame="):
            m = _RE_FRAME.search(line)
            if m:
                n = int(m.group(1))
                return self._frac(n), self._detail(n)
        elif line.startswith("out_time_us="):
            m = _RE_TIME_US.search(line)
            if m:
                self.t = int(m.group(1)) / 1e6
                return self._frac(self.t), self._detail(None)
        elif line.startswith("speed="):
            m = _RE_SPEED.search(line)
            if m:
                self.speed = f"{m.group(1)}x"
        elif line.startswith("total_size="):
            m = _RE_SIZE.search(line)
            if m:
                self.size = int(m.group(1))
        elif line.startswith("progress=end"):
            return 1.0, self._detail(None)
        return None, ""

    def _frac(self, n):
        return min(n / self.total, 1.0) if self.total > 0 else 0.0

    def _detail(self, frames):
        if self.kind == "frame":
            return f"帧 {frames}/{int(self.total)}"
        return "  ".join(x for x in (
            self.speed, human_mb(self.size) if self.size else "") if x)


class Job(QtCore.QThread):
    """按顺序跑若干条命令，任一条失败或被取消就停下并清掉剩余输出。

    必须无父对象：Qt 对已有父对象的 QObject 调 moveToThread 会静默失败。
    这里用 QThread 子类，run() 在本线程执行，同样不碰任何控件。
    """

    log = QtCore.Signal(str)
    progress = QtCore.Signal(float, str)      # 0~1，详情文字
    done = QtCore.Signal(bool, object)        # 成功?, 产出列表或错误说明
    stage = QtCore.Signal(int, str, str)      # 第几步、run/ok/bad、补一句

    def __init__(self, steps, parent=None):
        """steps: [(标签, cmd, Meter, 输出路径, cwd 或 None)]"""
        super().__init__(parent)
        self.steps = list(steps)
        # 结果卡的时间线读这个；不跑命令的任务（对齐、变速）自己覆盖
        self.timeline = [s[0] or f"第 {i + 1} 步"
                         for i, s in enumerate(self.steps)]
        self._proc = None
        self._stop = False

    # ---- 主线程调用 ----
    def cancel(self):
        """置标志 + terminate：等当前一步结束，不强杀，免得写出损坏容器。"""
        if self._stop:
            return
        self._stop = True
        self.log.emit("正在取消…（等当前这一步结束）")
        p = self._proc
        if p is not None and p.poll() is None:
            p.terminate()

    @property
    def stopping(self):
        return self._stop

    # ---- 线程内 ----
    def run(self):
        outs = []
        n = len(self.steps)
        for k, (label, cmd, meter, out, cwd) in enumerate(self.steps):
            if self._stop:
                break
            if n > 1:
                self.log.emit(f"\n[{k + 1}/{n}] {out}")
            self.log.emit("$ " + " ".join(f'"{c}"' if " " in c else c
                                         for c in cmd))
            self.stage.emit(k, "run", "")
            rc, misc = self._run_one(cmd, meter, cwd, k / n, 1.0 / n)
            if self._stop:
                self.stage.emit(k, "bad", "已取消")
                self._drop(self.steps[k:])
                self.done.emit(False, "cancelled")
                return
            if rc != 0 or not os.path.exists(out):
                self.stage.emit(k, "bad", f"退出码 {rc}")
                for m in misc:
                    self.log.emit(m)
                self._drop(self.steps[k:])
                self.done.emit(False, f"ffmpeg 退出码 {rc}：{out}")
                return
            size = os.path.getsize(out) / 1e6
            outs.append(out)
            self.stage.emit(k, "ok", f"{size:.1f} MB")
            self.log.emit(f"完成 {out}  {size:.1f} MB")
            if label:
                self.log.emit(f"  {label}")
        self.done.emit(True, outs)

    def _run_one(self, cmd, meter, cwd, base=0.0, span=1.0):
        """跑一条命令，逐行喂给 meter；返回 (退出码, 非进度行)。

        base/span 是这条命令在**整体进度**里占的那一段：渲染队列一段成片
        有"渲帧 + 编码"两个阶段，各自要能插进所属那一项的区间。
        """
        try:
            proc = subprocess.Popen(
                cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                text=True, encoding="utf-8", errors="replace",
                cwd=cwd or None, creationflags=NO_WINDOW)
        except OSError as e:
            self.log.emit(f"无法启动 ffmpeg：{e}")
            return 127, []
        self._proc = proc
        misc = []
        for line in proc.stdout:
            line = line.strip()
            if not line:
                continue
            frac, detail = meter.feed(line)
            if frac is None:
                misc.append(line)
            else:
                self.progress.emit(base + frac * span, detail)
        rc = proc.wait()
        self._proc = None
        return rc, misc

    def _drop(self, steps):
        """取消/失败留下的是不可播放的半成品，删掉免得被误当成交付物。"""
        for _label, _cmd, _meter, out, _cwd in steps:
            try:
                if os.path.exists(out):
                    os.remove(out)
                    self.log.emit(f"已删除未完成的 {out}")
            except OSError as e:
                self.log.emit(f"删除失败 {out}: {e}")
