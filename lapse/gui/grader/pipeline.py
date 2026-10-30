"""图片读写 + 后台调色线程。"""
import traceback

import numpy as np
import cv2
from PySide6 import QtCore, QtGui

from lapse import grade as gc
from lapse import imio

WORK_LONG = 2200          # 精修预览工作图长边：兼顾手感与细节
PROXY_LONG = 900          # 拖动中的草图长边：像素只有精修的 1/6，滑块才追得上手
SCOPE_STEP = 3            # 示波器统计的降采样步长（直方图本来就是分箱）


def load_float(path):
    """读图为 float32 RGB [0,1]；支持中文/空格路径。失败返回 None。

    统一按 16bit 解码：RAW 与 16bit PNG/TIFF 都能保留高位深，8bit 源
    只会多一次精确转换，不会损失信息。
    """
    try:
        im = imio.decode_image(path, bps=16, half_size=True)
    except Exception:
        return None
    if im is None:
        return None
    scale = 65535.0 if im.dtype == np.uint16 else 255.0
    return cv2.cvtColor(im, cv2.COLOR_BGR2RGB).astype(np.float32) / scale


def load_u8(path):
    """读图为 uint8 RGB（原分辨率，占内存小，按需取区域）。"""
    if imio.is_raw(path):
        try:
            im = imio.decode_image(path, bps=8, half_size=True)
        except Exception:
            return None
        return None if im is None else cv2.cvtColor(im, cv2.COLOR_BGR2RGB)
    try:
        buf = np.fromfile(path, dtype=np.uint8)
    except OSError:
        return None
    if buf.size == 0:
        return None
    im = cv2.imdecode(buf, cv2.IMREAD_COLOR)
    if im is None:
        return None
    return cv2.cvtColor(im, cv2.COLOR_BGR2RGB)


def decode_u8(buf):
    """字节流版 load_u8：导入预取给的是一帧 PNG 字节，不用再绕一趟磁盘。"""
    if not buf:
        return None
    im = cv2.imdecode(np.frombuffer(buf, np.uint8), cv2.IMREAD_COLOR)
    if im is None:
        return None
    return cv2.cvtColor(im, cv2.COLOR_BGR2RGB)


def fit_long(img, long_side=WORK_LONG):
    h, w = img.shape[:2]
    m = max(h, w)
    if m <= long_side:
        return img
    s = long_side / m
    nh, nw = int(round(h * s)), int(round(w * s))
    return cv2.resize(img, (nw, nh), interpolation=cv2.INTER_AREA)


def proxy_of(work, long_side=PROXY_LONG):
    """从 float32 工作图再缩一档，得到拖动滑块时用的草图。"""
    h, w = work.shape[:2]
    m = max(h, w)
    if m <= long_side:
        return work
    s = long_side / m
    return cv2.resize(work, (int(round(w * s)), int(round(h * s))),
                      interpolation=cv2.INTER_AREA)


def to_qimage_u8(arr_u8):
    h, w = arr_u8.shape[:2]
    a = np.ascontiguousarray(arr_u8)
    img = QtGui.QImage(a.data, w, h, 3 * w, QtGui.QImage.Format.Format_RGB888)
    return img.copy()          # detach，脱离 numpy 缓冲区


def thumb_u8(path, size=128, p=None):
    im = load_u8(path)
    if im is None:
        return None
    if p is not None and gc.changes(p):
        f = fit_long(im.astype(np.float32) / 255.0, size * 2)
        im = (np.clip(gc.apply(f, p), 0, 1) * 255).astype(np.uint8)
    h, w = im.shape[:2]
    s = size / max(h, w)
    return cv2.resize(im, (int(w * s), int(h * s)),
                      interpolation=cv2.INTER_AREA)


def thumb(path, size=128, p=None):
    im = thumb_u8(path, size, p)
    return None if im is None else to_qimage_u8(im)


class Job:
    __slots__ = ("img", "p", "token", "fast")

    def __init__(self, img, p, token, fast=False):
        self.img, self.p, self.token = img, p, token
        self.fast = fast


class Result:
    """一次渲染的产物：给视图的 QImage + 给示波器的降采样 uint8。

    示波器若拿全尺寸数组，直方图会在 GUI 线程的 paintEvent 里算掉几十毫秒，
    拖动时正好卡在那儿；分箱统计用 1/9 的像素看不出区别。
    """
    __slots__ = ("token", "image", "array")

    def __init__(self, token, image, array):
        self.token, self.image, self.array = token, image, array


class Grader(QtCore.QObject):
    """单线程渲染队列：只保留最新一帧请求，避免拖滑块时排队堆积。"""

    done = QtCore.Signal(object)     # Result
    failed = QtCore.Signal(str)      # 异常摘要：渲染线程不能死，界面要能解释

    def __init__(self):
        super().__init__()          # 不能有父对象，否则 moveToThread 会失败
        self._mutex = QtCore.QMutex()
        self._cond = QtCore.QWaitCondition()
        self._job = None
        self._stop = False
        self._token = 0
        self.thread = QtCore.QThread()
        self.moveToThread(self.thread)
        self.thread.started.connect(self.loop)
        self.thread.start()

    def submit(self, img, p, fast=False):
        with QtCore.QMutexLocker(self._mutex):
            self._token += 1
            self._job = Job(img, p, self._token, fast)
            self._cond.wakeOne()
        return self._token

    def loop(self):
        if QtCore.QThread.currentThread() is not self.thread:
            raise RuntimeError("Grader.loop 跑在了 GUI 线程：moveToThread 未生效")
        while True:
            with QtCore.QMutexLocker(self._mutex):
                while self._job is None and not self._stop:
                    self._cond.wait(self._mutex)
                if self._stop:
                    return
                job, self._job = self._job, None
            try:
                out = gc.apply(job.img, job.p, fast=job.fast)
                u8 = (out * 255.0 + 0.5).astype(np.uint8)
                scope = np.ascontiguousarray(u8[::SCOPE_STEP, ::SCOPE_STEP])
                res = Result(job.token, to_qimage_u8(u8), scope)
            except Exception:
                # 一个坏参数不该让整条渲染线程静默消失：报错后继续等下一条
                lines = traceback.format_exc().strip().splitlines()
                self.failed.emit(lines[-1] if lines else "渲染失败")
                continue
            self.done.emit(res)

    def shutdown(self):
        with QtCore.QMutexLocker(self._mutex):
            self._stop = True
            self._cond.wakeAll()
        self.thread.quit()
        self.thread.wait(3000)
