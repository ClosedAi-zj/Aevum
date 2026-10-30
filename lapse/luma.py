# -*- coding: utf-8 -*-
"""逐帧亮度测量：胶片条曲线、序列 QA、曝光台阶的共同输入。

骨架与 align.measure_offsets 同构（多进程池 + initializer + 小批量短路），
只是 worker 的输出从 (dx, dy) 换成亮度统计。亮度用 gamma 编码的灰度均值，
与去闪烁模块的线性域测光相互独立：这里要的是「看起来多亮」，用来画曲线和
找离群帧；去闪烁要的是可叠加的 EV，需要线性域。
"""
from multiprocessing import Pool, cpu_count

import cv2
import numpy as np

from lapse import imio

SMALL_BATCH = 12          # 同 align.py：Windows spawn 起池比算图还贵
_G = {}


def _init(long_side):
    _G["long"] = int(long_side)
    cv2.setNumThreads(1)      # 池里每个进程单线程，否则 N 进程 × M 线程互相抢


def measure_bgr(im, long_side=512):
    """返回 (mean, median, center, sharp)；空图返回 None。

    mean/median/center 在 0~1 的 gamma 灰度上；sharp 是拉普拉斯方差，
    用于 QA 找虚焦帧。中心区取中间三分之一，避开暗角和画面边缘。
    """
    if im is None or getattr(im, "size", 0) == 0:
        return None
    if im.dtype == np.uint16:
        im = (im >> 8).astype(np.uint8)
    elif im.dtype != np.uint8:
        im = np.clip(im, 0, 255).astype(np.uint8)
    h, w = im.shape[:2]
    s = float(long_side) / max(h, w, 1)
    if s < 1.0:
        im = cv2.resize(im, (max(1, int(round(w * s))),
                             max(1, int(round(h * s)))),
                        interpolation=cv2.INTER_AREA)
    gray = cv2.cvtColor(im, cv2.COLOR_BGR2GRAY)
    lum = gray.astype(np.float32) / 255.0
    H, W = lum.shape
    cy, cx = H // 3, W // 3
    center = lum[cy:2 * cy, cx:2 * cx]
    sharp = float(cv2.Laplacian(gray, cv2.CV_32F).var())
    return (float(lum.mean()), float(np.median(lum)),
            float(center.mean()) if center.size else float(lum.mean()),
            sharp)


def _worker(task):
    i, path, long_side = task
    try:
        img = imio.decode_image(path, bps=8, half_size=True)
    except Exception as e:
        return i, None, f"解码失败: {e}"
    if img is None:
        return i, None, "读取失败"
    v = measure_bgr(img, long_side)
    if v is None:
        return i, None, "空图"
    return i, v, "OK"


def measure_luma(files, long_side=512, workers=0, on_progress=None,
                 cancel=None):
    """返回 (luma, issues)。luma[i] = (mean, median, center, sharp) 或 None。

    cancel()->bool 为真时尽快收手并返回 (None, issues)；进程池会被终止，
    不会等剩下的帧跑完。
    """
    files = list(files)
    n = len(files)
    luma = [None] * n
    issues = []
    if n == 0:
        return luma, issues
    tasks = [(i, files[i], long_side) for i in range(n)]
    done = 0

    def consume(r):
        nonlocal done
        i, v, msg = r
        done += 1
        if v is None:
            issues.append((i, msg))
        else:
            luma[i] = v
            if msg != "OK":
                issues.append((i, msg))
        if on_progress:
            on_progress(done, n)

    if n <= SMALL_BATCH:
        old = cv2.getNumThreads()
        _init(long_side)
        try:
            for task in tasks:
                if cancel is not None and cancel():
                    return None, issues
                consume(_worker(task))
        finally:
            cv2.setNumThreads(old)
        return luma, issues

    w = min(workers if workers > 0 else max(1, cpu_count() - 1), n)
    with Pool(w, initializer=_init, initargs=(long_side,)) as pool:
        for r in pool.imap_unordered(_worker, tasks, chunksize=2):
            consume(r)
            if cancel is not None and cancel():
                pool.terminate()
                return None, issues
    return luma, issues


def means_of(luma):
    """luma -> 逐帧 mean 数组，读不出的帧为 NaN。"""
    return np.array([x[0] if x else np.nan for x in luma], dtype=np.float64)


def sharp_of(luma):
    """luma -> 逐帧清晰度数组，读不出的帧为 NaN。"""
    return np.array([x[3] if x else np.nan for x in luma], dtype=np.float64)
