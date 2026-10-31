"""播放引擎：把视频或图片序列按帧喂给界面，用于实时预览。

只有一条规则：GUI 侧的定时器每 tick 给解码线程"记一笔账"（pending += 1），
解码线程只有在欠账时才干活。这样播放速率 = min(素材帧率, 解码速率)，
解码跟不上时是丢帧而不是攒一堆过期帧，CPU 也不会白烧。

两种来源共用同一个 Reader 协议：
    VideoSource  ffmpeg 快进定位 + rawvideo 管道连续读（顺序解码才快）
    ListSource   按顺序读图片列表（延时序列的"按序播放"）
"""
import subprocess
import threading

import numpy as np
from PySide6 import QtCore

from reference import ff as ffmod
from reference.gui.grader import pipeline

PLAY_LONG = 1280       # 播放预览的长边：够看清构图与运动，又追得上帧率
DISCARD_MAX = 8        # 往前挪这么几帧以内就直接丢帧，不值得重启 ffmpeg


def _clamp(v, lo, hi):
    return max(lo, min(hi, v))


class ListSource:
    """图片序列：一帧就是一张图，顺序读。"""

    def __init__(self, paths, long_side=PLAY_LONG):
        self.paths = list(paths)
        self.long = long_side

    @property
    def count(self):
        return len(self.paths)

    @property
    def fps(self):
        return 25.0

    def open(self, index):
        return _ListReader(self, index)


class _ListReader:
    def __init__(self, src, index=0):
        self.src = src
        self.i = _clamp(index, 0, max(0, src.count - 1))

    def seek(self, index):
        self.i = _clamp(index, 0, max(0, self.src.count - 1))

    def read(self):
        """返回 RGB uint8；读不出来的那张直接跳过，别让一帧坏图卡住整段。"""
        while self.i < self.src.count:
            path = self.src.paths[self.i]
            self.i += 1
            im = pipeline.load_u8(path)
            if im is None:
                continue
            return np.ascontiguousarray(pipeline.fit_long(im, self.src.long),
                                        dtype=np.uint8)
        return None

    def alive(self):
        return True

    def close(self):
        pass


class VideoSource:
    """ffmpeg 解码：-ss 快进到目标时间后连续读 rawvideo，顺序解帧才跟得上。"""

    def __init__(self, ff, path, info, long_side=PLAY_LONG):
        self.ff = ff
        self.path = path
        self.info = info
        self.long = long_side
        self.fps = float(info.get("fps") or 25.0) or 25.0
        if self.fps > 240:              # 个别流探到的帧率离谱，收一下
            self.fps = 25.0
        dur = float(info.get("dur") or 0.0)
        self._count = int(round(dur * self.fps)) if dur else 0

    @property
    def count(self):
        return self._count

    def open(self, index):
        return _VideoReader(self, index)


class _VideoReader:
    def __init__(self, src, index=0):
        self.src = src
        self.proc = None
        self.w = self.h = 0
        self.i = 0                      # 下一帧的序号
        self._start(max(0, index))

    def _size(self):
        # 按显示尺寸算：非方形像素直接按存储尺寸缩放会把画面压扁
        return ffmod.fit_long_size(*ffmod.disp_size(self.src.info), self.src.long)

    def _start(self, index):
        self.close()
        self.w, self.h = self._size()
        t = index / self.src.fps
        cmd = [self.src.ff, "-v", "error", "-ss", f"{t:.3f}", "-i",
               self.src.path, "-an", "-sn", "-vf",
               f"scale={self.w}:{self.h}", "-f", "rawvideo",
               "-pix_fmt", "rgb24", "-"]
        try:
            self.proc = subprocess.Popen(
                cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                bufsize=self.w * self.h * 3,
                creationflags=ffmod.NO_WINDOW)
        except OSError:
            self.proc = None
        self.i = index

    def seek(self, index):
        index = max(0, index)
        ahead = index - self.i
        if 0 <= ahead <= DISCARD_MAX:
            for _ in range(ahead):
                if self.read() is None:
                    self._start(index)
                    return
            return
        self._start(index)

    def read(self):
        if self.proc is None:
            return None
        need = self.w * self.h * 3
        buf = bytearray()
        while len(buf) < need:
            chunk = self.proc.stdout.read(need - len(buf))
            if not chunk:
                self.proc = None
                return None
            buf += chunk
        self.i += 1
        return np.frombuffer(bytes(buf), dtype=np.uint8).reshape(
            self.h, self.w, 3)

    def alive(self):
        return self.proc is not None

    def close(self):
        if self.proc is not None:
            try:
                self.proc.stdout.close()
            except OSError:
                pass
            try:
                self.proc.kill()
            except OSError:
                pass
            self.proc.wait()
            self.proc = None


class _Ctl:
    """解码线程与 GUI 之间的那点共享状态。"""

    def __init__(self):
        self.cv = threading.Condition()
        self.pending = 0
        self.jump = None
        self.stop = False

    def credit(self, n=1):
        with self.cv:
            # 欠账最多记 2 帧：解码慢时后面的 tick 直接丢弃，不攒过期帧
            self.pending = min(self.pending + n, 2)
            self.cv.notify_all()

    def request_jump(self, index):
        with self.cv:
            self.jump = index
            self.pending = max(self.pending, 1)
            self.cv.notify_all()

    def halt(self):
        with self.cv:
            self.stop = True
            self.cv.notify_all()


class Player(QtCore.QObject):
    """播放控制器：定时器管节奏，解码线程管出帧。"""

    frame = QtCore.Signal(int, object)      # 序号, RGB uint8
    ended = QtCore.Signal()

    def __init__(self, parent=None):
        super().__init__(parent)
        self.source = None
        self.count = 0
        self.fps = 25.0
        self.index = 0
        self.speed = 1.0
        self.loop = True
        self.playing = False
        self._advanced = False
        self._gen = 0               # 换源后旧线程还在路上的帧一律丢弃
        self._ctl = None
        self._thread = None
        self._retiring = []       # 换源时还没走完的旧线程，等它们自己收尾
        self._worker = None
        self._timer = QtCore.QTimer(self)
        self._timer.timeout.connect(lambda: self._ctl and self._ctl.credit())

    # ------------------------------------------------------------------ 线程
    def _start_thread(self):
        if self._thread is not None:
            return
        # moveToThread 对已有父对象的 QObject 会静默失败 -> 必须无父对象
        w = _Worker(self.source, self._ctl, self.index, self._gen)
        w.frame.connect(self._on_frame)
        w.exhausted.connect(self._on_exhausted)
        t = QtCore.QThread()
        w.moveToThread(t)
        t.started.connect(w.run)
        t.finished.connect(w.deleteLater)
        self._worker, self._thread = w, t
        t.start()

    def _stop_thread(self):
        if self._ctl is not None:
            self._ctl.halt()
        self._prune_threads()
        t = self._thread
        self._worker = self._thread = None
        if t is None:
            return
        t.quit()
        if not t.wait(1500):
            # 还堵在最后一帧的读上：这时候把 QThread 对象丢掉会触发
            # "destroyed while thread is still running" 直接崩，先养着
            self._retiring.append(t)

    def _prune_threads(self):
        for t in [x for x in self._retiring if x.isFinished()]:
            self._retiring.remove(t)

    # ------------------------------------------------------------------ 源
    def set_source(self, source, index=0, prime=True):
        """换素材：先停线程，再建新的控制状态。

        prime=False 只把指针放好，不起解码线程——载入一张静图时想要的是
        高分辨率工作图，等用户按播放或拖时间轴再出帧。
        """
        self.pause()
        self._stop_thread()
        self._gen += 1              # 停止线程前已发出的帧还在队列里，按代作废
        self.source = source
        self.count = source.count if source else 0
        self.fps = source.fps if source else 25.0
        self._ctl = _Ctl()
        self._advanced = False
        if prime:
            self.seek(index)
        else:
            self.park(index)

    def park(self, index):
        """序列里换到另一张：线程还活着就跟着跳，没起就只挪指针。"""
        self.index = _clamp(int(index), 0, max(0, self.count - 1))
        if self._thread is not None and self._ctl is not None:
            self._ctl.request_jump(self.index)

    def interval(self):
        return max(10, int(round(1000.0 / max(1.0, self.fps * self.speed))))

    def set_speed(self, mult):
        self.speed = max(0.05, float(mult))
        if self.playing:
            self._timer.start(self.interval())

    # ------------------------------------------------------------------ 传输
    def play(self):
        if self.source is None or self.count == 0:
            return
        if self.index >= self.count - 1 and not self.loop:
            self.seek(0)
        self._start_thread()
        self.playing = True
        self._ctl.credit()
        self._timer.start(self.interval())

    def pause(self):
        self.playing = False
        self._timer.stop()

    def toggle(self):
        self.pause() if self.playing else self.play()

    def seek(self, index):
        index = _clamp(int(index), 0, max(0, self.count - 1))
        self.index = index
        if self.source is None or self._ctl is None:
            return
        if self._thread is None:
            self._start_thread()
        self._ctl.request_jump(index)

    def step(self, delta=1):
        if self.count:
            self.seek(self.index + delta)

    def shutdown(self):
        self.pause()
        self._stop_thread()
        for t in list(self._retiring):
            t.wait(3000)
        self._retiring = []

    # ------------------------------------------------------------------ 回调
    def _on_frame(self, gen, index, arr):
        if gen != self._gen:
            return
        # 换列表瞬间旧线程还可能送来末尾帧，索引必须留在有效范围
        index = _clamp(int(index), 0, max(0, self.count - 1))
        self.index = index
        self._advanced = True
        self.frame.emit(index, arr)

    def _on_exhausted(self, gen):
        """读到尽头：循环就回到 0，不循环就停。

        连着两次尽头之间一帧都没出（素材全读不出来）就直接停，
        否则会在两个线程之间来回重启解码器空转。
        """
        if gen != self._gen:
            return
        if not self._advanced:
            self.pause()
            self.ended.emit()
            return
        self._advanced = False
        if self.loop:
            self.seek(0)
        else:
            self.pause()
            self.ended.emit()


class _Worker(QtCore.QObject):
    """跑在工作线程里的取帧循环：只在 GUI 欠账时解码。"""

    frame = QtCore.Signal(int, int, object)     # 代, 序号, RGB
    exhausted = QtCore.Signal(int)

    def __init__(self, source, ctl, index=0, gen=0):
        super().__init__()
        self.source = source
        self.ctl = ctl
        self.gen = gen
        self.reader = None
        self.i = _clamp(int(index), 0, max(0, source.count - 1))

    def run(self):
        while True:
            with self.ctl.cv:
                while (self.ctl.pending <= 0
                       and self.ctl.jump is None
                       and not self.ctl.stop):
                    self.ctl.cv.wait()
                if self.ctl.stop:
                    break
                jump = self.ctl.jump
                self.ctl.jump = None
                if jump is None:
                    self.ctl.pending -= 1
                else:
                    self.ctl.pending = 0
            if jump is not None:
                self.i = jump
                if self.reader is None:
                    self.reader = self.source.open(jump)
                else:
                    self.reader.seek(jump)
            if self.reader is None:
                # 线程刚起来时没有 jump 也要能出帧：从当前指针打开 reader
                self.reader = self.source.open(self.i)
            arr = self.reader.read()
            if arr is None:
                # 到底了：交给 Player 决定循环还是停
                self.reader = None
                self.exhausted.emit(self.gen)
                with self.ctl.cv:
                    self.ctl.pending = 0
                continue
            self.frame.emit(self.gen, self.i, arr)
            self.i += 1
        if self.reader is not None:
            self.reader.close()
