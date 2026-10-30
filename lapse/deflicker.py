# -*- coding: utf-8 -*-
"""延时序列去闪烁：逐帧测光 + 稳健平滑，输出曝光偏移（EV）。

只处理亮度的慢趋势：中值滤波压掉单帧跳变，再高斯平滑避免校正后出现台阶。
测光在 sRGB 反解后的线性域做，偏移单位正好等于调色台的曝光 EV；
日落、云影这类真实亮度变化会被平滑窗口保留，不会被当成闪烁抹平。

除了整帧亮度，也能按 R/G/B 三个通道各测各的（分通道去闪）：纯亮度去闪压得住
明暗跳，压不住彩色场景（蓝天渐变、霓虹）的色闪。还可以跑两遍（多遍）：先用
3 帧短窗挑出逐帧交替的闪烁，再用设定窗口把被短窗啃掉的真实渐变添回去。
"""
import math

import cv2
import numpy as np

from lapse import imio

SCAN_LONG = 320          # 测光只读缩略图：整段序列的耗时主要在这里省下来
MAX_EV = 3.0             # 单帧最多补几档：再大就不是闪烁而是坏帧了
WINDOW_MIN = 3
WINDOW_MAX = 199
LUMA_RGB = np.array([0.2126, 0.7152, 0.0722], dtype=np.float64)


def _srgb_to_linear(v):
    return np.where(v <= 0.04045, v / 12.92,
                    ((v + 0.055) / 1.055) ** 2.4)


_SRGB_LUT = _srgb_to_linear(np.arange(256, dtype=np.float64) / 255.0
                            ).astype(np.float32)
_LUMA = np.array([0.0722, 0.7152, 0.2126], dtype=np.float32)   # BGR


def _linear_rgb(im):
    """任意 BGR 图 -> 线性光 RGB float32，长边缩到 SCAN_LONG。失败返回 None。"""
    if im is None or getattr(im, "size", 0) == 0:
        return None
    if im.ndim == 2:
        im = np.stack([im] * 3, axis=-1)
    if im.dtype == np.uint16:
        im = (im >> 8).astype(np.uint8)
    elif im.dtype != np.uint8:
        im = np.clip(im, 0, 255).astype(np.uint8)
    h, w = im.shape[:2]
    m = max(h, w)
    if m > SCAN_LONG:
        s = SCAN_LONG / m
        im = cv2.resize(im, (max(1, int(round(w * s))),
                             max(1, int(round(h * s)))),
                        interpolation=cv2.INTER_AREA)
    return _SRGB_LUT[im][..., ::-1]          # BGR -> RGB


def _clipped_mean(a):
    """1%~99% 截尾均值：抗坏点与高光，压不住整帧的明暗跳。"""
    lo, hi = np.percentile(a, (1.0, 99.0))
    sel = a[(a >= lo) & (a <= hi)]
    return float(sel.mean()) if sel.size else float(a.mean())


def measure_rgb(im):
    """一张 BGR 图的 R/G/B 线性均值（3,）。空图返回 None。"""
    lin = _linear_rgb(im)
    if lin is None:
        return None
    flat = lin.reshape(-1, 3)
    return np.array([_clipped_mean(flat[:, c]) for c in range(3)],
                    dtype=np.float64)


def measure_bgr(im):
    """一张 BGR uint8/uint16 图的线性亮度均值（1%~99% 截尾，抗坏点/高光）。

    返回 None 表示空图；全黑帧返回 0（后续会当作无效帧跳过）。
    """
    if im is None or getattr(im, "size", 0) == 0:
        return None
    if im.ndim == 2:
        im = np.stack([im] * 3, axis=-1)
    if im.dtype == np.uint16:
        im = (im >> 8).astype(np.uint8)
    elif im.dtype != np.uint8:
        im = np.clip(im, 0, 255).astype(np.uint8)
    h, w = im.shape[:2]
    m = max(h, w)
    if m > SCAN_LONG:
        s = SCAN_LONG / m
        im = cv2.resize(im, (max(1, int(round(w * s))),
                             max(1, int(round(h * s)))),
                        interpolation=cv2.INTER_AREA)
    lin = _SRGB_LUT[im]
    lum = lin @ _LUMA
    lo, hi = np.percentile(lum, (1.0, 99.0))
    sel = lum[(lum >= lo) & (lum <= hi)]
    return float(sel.mean()) if sel.size else float(lum.mean())


def _odd_window(window, n):
    try:
        w = int(round(float(window)))
    except (TypeError, ValueError):
        w = 15
    w = min(max(w, WINDOW_MIN), WINDOW_MAX)
    if w % 2 == 0:
        w += 1
    if w > n:
        w = n if n % 2 else n - 1
    return w if w >= WINDOW_MIN else 0


def _median(values, window):
    """边界复制的中值滤波，不依赖 scipy。"""
    if window < 3:
        return values.copy()
    pad = window // 2
    padded = np.pad(values, pad, mode="edge")
    blocks = np.lib.stride_tricks.sliding_window_view(padded, window)
    return np.median(blocks, axis=1)


def _gauss(values, sigma):
    radius = max(1, int(round(sigma * 3)))
    x = np.arange(-radius, radius + 1, dtype=np.float64)
    k = np.exp(-0.5 * (x / sigma) ** 2)
    k /= k.sum()
    padded = np.pad(values, radius, mode="edge")
    return np.convolve(padded, k, mode="valid")


def _as_matrix(means):
    """(n,) 或 (n,3) -> (n,k)。每列各自插值补洞；三列都好的帧才算有效帧。"""
    a = np.asarray(means, dtype=np.float64)
    if a.ndim == 1:
        a = a[:, None]
    else:
        a = a.reshape(a.shape[0], -1)
    if a.size == 0:
        return np.zeros((0, 1)), np.zeros(0, dtype=bool)
    idx = np.arange(a.shape[0])
    ok = np.isfinite(a) & (a > 0.0)
    filled = a.copy()
    for c in range(a.shape[1]):
        v = ok[:, c]
        if v.any():
            filled[~v, c] = np.interp(idx[~v], idx[v], a[v, c])
    return filled, ok.all(axis=1)


def _pass_windows(w, multi):
    """多遍的窗口表。

    顺序是短窗在前：3 帧中值专挑逐帧交替的闪烁，但它会顺手啃掉一点真实渐变，
    紧接着用用户那个长窗再扫一遍，把慢趋势添回去。反过来（长->短）没用——
    第一遍之后残差本身就是平滑的，短窗再扫什么都找不到。
    """
    if not multi or w <= 3:
        return [w]
    return [3, w]


def _offsets(filled, valid, w, amount, multi):
    """逐列求 EV 偏移。filled/valid 由 _as_matrix 给出。"""
    log = np.log2(np.maximum(filled, 1e-7))
    ev = np.zeros_like(log)
    for c in range(log.shape[1]):
        col = log[:, c]
        acc = np.zeros_like(col)
        for win in _pass_windows(w, multi):
            resid = col + acc
            target = _gauss(_median(resid, win), max(0.8, win / 4.0))
            acc = np.clip(acc + (target - resid) * amount, -MAX_EV, MAX_EV)
        ev[:, c] = acc
    ev[~valid] = 0.0
    return ev


def _jitter(log_values):
    if log_values.size < 2:
        return 0.0
    return float(np.percentile(np.abs(np.diff(log_values)), 95))


def offsets_from_lumas(means, window=15, strength=100.0, multi=False):
    """逐帧亮度 -> 曝光偏移数组（EV，正=提亮）。

    means 给 (n,) 是亮度，给 (n,3) 是 R/G/B 分通道，返回同形状。
    multi=True 才做第二遍短窗，亮度单通道时一般用不上。
    """
    a = np.asarray(means, dtype=np.float64)
    flat = a.ndim == 1
    filled, valid = _as_matrix(a)
    n = filled.shape[0]
    if n == 0:
        return np.zeros(0, dtype=np.float64) if flat else np.zeros((0, 3))
    try:
        amount = float(strength) / 100.0
    except (TypeError, ValueError):
        amount = 1.0
    w = _odd_window(window, n)
    if w == 0 or int(valid.sum()) < 3:
        return np.zeros(n, dtype=np.float64) if flat else np.zeros((n, 3))
    ev = _offsets(filled, valid, w, min(max(amount, 0.0), 1.0), multi)
    return ev[:, 0] if flat else ev


def analyze_means(means, window=15, strength=100.0, multi=False):
    """返回 (offsets, stats)；stats 里的抖动单位是 EV（相邻帧 P95 差）。

    分通道时 offsets 是 (n,3)，stats 额外给 color_before/after：
    三个通道里最抖的那个，校正前后的 P95。
    """
    a = np.asarray(means, dtype=np.float64)
    offsets = offsets_from_lumas(a, window, strength, multi)
    filled, valid = _as_matrix(a)
    chan = filled.shape[1] > 1
    log = np.log2(np.maximum(filled, 1e-7))
    after = log + offsets if chan else None
    if chan:
        lum = np.clip(filled, 1e-7, None) @ LUMA_RGB
        lum_after = np.clip(np.exp2(after), 1e-7, None) @ LUMA_RGB
        before = float(np.max([_jitter(np.log2(np.maximum(filled[:, c], 1e-7)))
                               for c in range(3)])) if valid.any() else 0.0
        after_c = float(np.max([_jitter(after[:, c]) for c in range(3)])) \
            if valid.any() else 0.0
        jb = _jitter(np.log2(np.maximum(lum, 1e-7)))
        ja = _jitter(np.log2(np.maximum(lum_after, 1e-7)))
    elif filled.size >= 2 and valid.any():
        l = np.log2(np.maximum(filled[:, 0], 1e-7))
        jb = _jitter(l)
        ja = _jitter(l + offsets)
        before = after_c = 0.0
    else:
        jb = ja = before = after_c = 0.0
    vals = offsets.reshape(-1)[np.abs(offsets.reshape(-1)) > 1e-9]
    stats = {
        "jitter_before": round(jb, 4),
        "jitter_after": round(ja, 4),
        "color_before": round(before, 4),
        "color_after": round(after_c, 4),
        "channels": bool(chan),
        "max_ev": round(float(np.max(np.abs(offsets))) if offsets.size else 0.0, 3),
        "mean_abs_ev": round(float(np.mean(np.abs(vals))) if vals.size else 0.0, 3),
        "frames": int(filled.shape[0]),
        "valid": int(valid.sum()),
    }
    return offsets, stats


def measure_path(path, channels=False):
    """读一张图并测光；读不了返回 None，绝不抛异常拖垮整段分析。"""
    try:
        im = imio.decode_image(path, bps=8, half_size=True)
    except Exception:
        return None
    return measure_rgb(im) if channels else measure_bgr(im)


def scan(paths, window=15, strength=100.0, progress=None, cancel=None,
         channels=False, multi=False):
    """逐帧测光并生成偏移。

    progress(frac, text) 与 cancel()->bool 都由调用方提供：核心层不依赖 Qt。
    取消时返回 None；无法读取的帧亮度置 NaN，插值后参与平滑，自身偏移为 0。
    channels=True 时 means/offsets 每帧都是 [R,G,B] 三个 EV。
    """
    paths = list(paths)
    n = len(paths)
    hole = [float("nan")] * 3 if channels else float("nan")
    means, bad = [], []
    for i, path in enumerate(paths):
        if cancel is not None and cancel():
            return None
        v = measure_path(path, channels)
        if v is None:
            bad.append(path)
            means.append(hole if not channels else list(hole))
        else:
            means.append(v.tolist() if channels else v)
        if progress is not None:
            progress((i + 1) / max(n, 1), f"{i + 1}/{n}")
    offsets, stats = analyze_means(means, window, strength, multi)
    stats["bad"] = len(bad)
    return {"means": means, "offsets": offsets.tolist(),
            "stats": stats, "bad": bad}


def _num(v):
    try:
        v = float(v)
    except (TypeError, ValueError):
        return float("nan")
    return v if math.isfinite(v) and v > 0 else float("nan")


def sanitize_means(raw, n):
    """旁车读取：长度对齐、有限正数；坏值留 NaN 让平滑层再补。

    分通道时每项是 [R,G,B]（读不了的帧是 [None, None, None]），形状照原样留。
    """
    if not isinstance(raw, (list, tuple)):
        raw = []
    out = []
    for i in range(min(int(n), 200000)):
        v = raw[i] if i < len(raw) else None
        if isinstance(v, (list, tuple)):
            out.append([_num(v[c]) if c < len(v) else float("nan")
                        for c in range(3)])
        else:
            out.append(_num(v))
    return out
