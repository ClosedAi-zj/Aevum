# -*- coding: utf-8 -*-
"""重定时：变速曲线与运动模糊，纯函数、不含界面。

两件事共用同一张"输出帧 → 源帧位置"的映射：
  变速改的是这张映射的疏密（哪一段走得快），
  运动模糊改的是每张输出帧要混哪几帧源（快门开了多久）。
所以映射只依赖参数、能被单测钉死，渲染端照着取样就行。

位置是浮点源帧号（0 就是第一帧）。取样号只会一路向前，所以视频和图片序列
都能流式解码，不必把整条片子读进内存。
"""
import math

import numpy as np

OFF = "off"
MIX = "mix"
BLUR = "blur"
MODE_LABELS = ((OFF, "不混合（还是原来那张帧）"),
               (MIX, "邻帧混合（慢放不顿）"),
               (BLUR, "运动模糊（多帧平均）"))
MODE_DEF = OFF
TAP_CHOICES = (3, 5, 7, 9)
TAP_DEF = 5
SHAPE_BINS = 64            # 轮廓采样份数：够平滑，算平均时也不会抖
MIN_SPEED = 1e-3           # 倍速下限，防除零


def smoothstep(t):
    return t * t * (3.0 - 2.0 * t)


# 预设填的是起/中/止三个相对倍速，倍速旋钮在外面另算
SHAPE_PRESETS = (("匀速", (1.0, 1.0, 1.0)),
                 ("慢-快-慢", (0.5, 2.0, 0.5)),
                 ("快-慢-快", (2.0, 0.5, 2.0)),
                 ("逐渐加快", (0.6, 1.0, 1.7)),
                 ("逐渐放慢", (1.7, 1.0, 0.6)))


def speed_shape(a=1.0, m=1.0, b=1.0, bins=SHAPE_BINS):
    """起点/中点/终点三个相对倍速之间的平滑轮廓。

    两段都用 smoothstep，锚点处导数为 0：倍速不会在某个瞬间跳一下，
    画面就不会在切换点突然抖一帧。
    """
    if min(a, m, b) <= 0:
        raise ValueError(f"倍速必须大于 0：{a} / {m} / {b}")
    u = np.linspace(0.0, 1.0, max(3, int(bins)))
    s = np.empty_like(u)
    left = u <= 0.5
    t = smoothstep(u[left] / 0.5)
    s[left] = a + (m - a) * t
    right = ~left
    t = smoothstep((u[right] - 0.5) / 0.5)
    s[right] = m + (b - m) * t
    return s


def retime(src_n, src_fps, out_fps, shape=None, rate=1.0):
    """按速度轮廓重定时，返回 (源位置数组, 输出帧数, 成片时长)。

    第 k 帧源要花 1/(src_fps×倍速) 秒放完，累加起来就是成片时间轴；
    按 out_fps 均匀取点反查，就得到每张输出帧落在源片哪一帧。
    成片时长是算出来的，不是设出来的——轮廓越往后快，片子就越短。
    """
    src_n = int(src_n)
    if src_n < 2:
        raise ValueError(f"源素材只有 {src_n} 帧，重定时没有意义")
    if src_fps <= 0 or out_fps <= 0:
        raise ValueError(f"帧率不合法：源 {src_fps} 出 {out_fps}")
    if shape is None:
        s = np.full(src_n, float(rate))
    else:
        u = (np.arange(src_n) + 0.5) / src_n          # 按帧中心取样
        xs = np.linspace(0.0, 1.0, len(shape))
        s = np.interp(u, xs, np.asarray(shape, dtype=float)) * float(rate)
    s = np.maximum(s, MIN_SPEED)
    dt = 1.0 / (float(src_fps) * s)
    total = float(dt.sum())
    out_n = max(2, int(round(total * float(out_fps))))
    # 第 k 帧源占 [cum[k], cum[k+1])，输出第 i 帧在 i/out_fps 这一刻取画面：
    # 这样"匀速 1× 且帧率不变"落在整数上（pos 就是 i），不会莫名给整段
    # 加半帧位移、把每一帧都糊成相邻两帧的平均。
    cum = np.concatenate(([0.0], np.cumsum(dt)))
    ts = np.arange(out_n) / float(out_fps)
    idx = np.clip(np.searchsorted(cum, ts, side="right") - 1, 0, src_n - 1)
    pos = idx + (ts - cum[idx]) / dt[idx]
    return np.clip(pos, 0.0, src_n - 1.0), out_n, total


def half_spans(pos):
    """每张输出帧在源片上跨过多少帧（半宽），运动模糊的窗口按它算。

    慢放时相邻输出帧只差零点几帧，窗口很窄、几乎不糊；快放时一下跳过好几帧，
    窗口自动变宽——这正好像真实快门：曝光期间被拍的东西走了多远就糊多少。
    """
    return np.gradient(np.asarray(pos, dtype=float)) * 0.5


def taps(pos, span, mode=MODE_DEF, count=TAP_DEF, strength=1.0, src_n=0):
    """一张输出帧由哪几张源帧按权重混成，返回 [(源帧号, 权重)]，权重和为 1。

    运动模糊 = 清晰的那一帧（邻帧混合的结果）按强度掺进窗口平均里：
    强度 100% 是全糊，0% 等于没开，中间值是"动的地方拖尾、静止的地方仍干净"。
    """
    hi = max(0, int(src_n) - 1) if src_n else 0
    p = min(max(float(pos), 0.0), float(hi)) if hi else max(0.0, float(pos))
    k = int(math.floor(p))
    f = p - k
    if mode == OFF:
        return [(_clamp(int(round(p)), hi), 1.0)]
    sharp = {}
    if mode == MIX or f > 0.0:
        _add(sharp, k, 1.0 - f, hi)
        _add(sharp, k + 1, f, hi)
    else:                                   # 只有运动模糊才摊开窗口
        _add(sharp, k, 1.0, hi)
    if mode != BLUR:
        return sorted(sharp.items())
    st = min(max(float(strength), 0.0), 1.0)
    if st <= 0.0:
        return sorted(sharp.items())
    n = max(2, int(count))
    d = max(0.0, float(span))
    if d <= 0.0:
        _add(sharp, int(round(p)), st, hi)
        return sorted(sharp.items())
    # 清晰那份按 (1-st) 保留，窗口平均占 st
    for i in sorted(sharp):
        sharp[i] *= (1.0 - st)
    w = st / n
    for i in range(n):
        _add(sharp, int(round(p - d + 2 * d * i / (n - 1))), w, hi)
    return sorted(sharp.items())


def _add(d, idx, w, hi):
    if w <= 0.0:
        return
    i = _clamp(idx, hi)
    d[i] = d.get(i, 0.0) + w


def _clamp(i, hi):
    return min(max(int(i), 0), hi) if hi else 0


def plan_taps(pos, src_n, mode=MODE_DEF, count=TAP_DEF, strength=1.0):
    """整段的取样表：每行 [(源帧号, 权重)]。权重各自归一，行内按源帧号升序。"""
    spans = half_spans(pos)
    rows = [taps(p, d, mode, count, strength, src_n)
            for p, d in zip(pos, spans)]
    return rows


def is_flat(shape, tol=0.01):
    """轮廓是否恒定倍速——只有恒定时音轨才能用 atempo 整条跟着变。"""
    if shape is None or len(shape) == 0:
        return True
    s = np.asarray(shape, dtype=float)
    return float(s.max() - s.min()) <= tol


def atempo(rate):
    """倍速 → ffmpeg 的 atempo 滤波器串（每级只接受 0.5~2，所以要串起来）。

    返回空串表示不用改速度。倍速落在浮点边界上时最后一级会差 1e-9 量级，
    听不出来，但别让链条少一级导致整条音轨差一倍。
    """
    r = float(rate)
    if r <= 0:
        return ""
    stages = []
    while r > 2.0 + 1e-9:
        stages.append(2.0)
        r /= 2.0
    while r < 0.5 - 1e-9:
        stages.append(0.5)
        r /= 0.5
    stages.append(round(r, 6))
    if len(stages) == 1 and abs(stages[0] - 1.0) < 1e-6:
        return ""
    return ",".join(f"atempo={s:g}" for s in stages)


def avg_speed(src_n, src_fps, total):
    """整段平均倍速：源片按原速放多久 ÷ 成片多长。"""
    if total <= 0:
        return 0.0
    return (float(src_n) / float(src_fps)) / float(total)
