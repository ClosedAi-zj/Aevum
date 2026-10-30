# -*- coding: utf-8 -*-
"""Holy Grail 曝光台阶：找出相机手动改 ISO/快门/光圈的帧，生成反向斜坡。

台阶来自相机为了追光而做的整档调整，成片里会表现为一帧之内突然亮/暗几档。
这里先抵消台阶处的跳变，再在随后若干帧里把这一档均匀放回去：画面连续，
整段日夜过渡的总曝光变化也保留下来（去的是台阶，不是渐变）。

EXIF 读不到时退回逐帧亮度推断：只看「跳完还站得住」的亮度变化，单帧闪烁
不算台阶。两条路的判据都只在相邻帧之间比较，不需要全局曝光基准。
"""
import math

import numpy as np

THR_EV = 0.5          # EXIF 相邻帧档差达到 0.5 EV 才算台阶
LUMA_THR = 0.4        # 亮度推断阈值（log2 亮度差）
HOLD = 3              # 台阶前后各看几帧，压掉单帧闪烁
RAMP = 12             # 默认用多少帧把台阶摊平
MAX_STEP_EV = 20.0    # 误读 EXIF 时的护栏，避免整段被一条假台阶带飞


def find_steps(ev_series, thr=THR_EV):
    """EXIF EV 序列 -> [(帧号, 档差)]；None 表示该帧读不到。"""
    out = []
    prev = None
    for i, v in enumerate(ev_series):
        if v is None:
            prev = None
            continue
        if prev is not None:
            d = float(v) - prev
            if abs(d) >= float(thr):
                out.append((i, round(d, 3)))
        prev = float(v)
    return out


def find_luma_steps(means, thr=LUMA_THR, hold=HOLD):
    """亮度序列 -> [(帧号, 档差)]；帧号是台阶后的第一帧。

    判据：跳变前后各取 hold 帧中值，两个平台确实分开了才算台阶。
    这样单帧闪烁（跳出去又回来）会被中值抵消，持续变亮的渐变也不会
    因为相邻帧差小而误报。
    """
    m = np.array([np.nan if v is None else float(v) for v in means],
                 dtype=np.float64)
    if m.size < 3:
        return []
    log = np.where(m > 0, np.log2(np.maximum(m, 1e-9)), np.nan)
    n = log.size
    hold = max(1, int(hold))
    thr = float(thr)
    out = []
    i = 1
    while i < n:
        jump = log[i] - log[i - 1]
        if not (np.isfinite(jump) and abs(jump) >= thr * 0.5):
            i += 1
            continue
        pre = log[max(0, i - hold):i]
        post = log[i:min(n, i + hold)]
        pre = pre[np.isfinite(pre)]
        post = post[np.isfinite(post)]
        if pre.size < 2 or post.size < 2:
            i += 1
            continue
        d = float(np.median(post) - np.median(pre))
        if abs(d) >= thr:
            out.append((i, round(d, 3)))
            i += hold
        else:
            i += 1
    return out


def ramp_offsets(steps, n, ramp=RAMP):
    """台阶 -> 逐帧曝光偏移（EV）。

    第 idx 帧先补 -d 把跳变抵消，随后 ramp 帧线性回到 0：相机那档曝光
    会在后面几帧里平滑地出现，而不是在一帧里跳出来。斜坡放在台阶之后
    而不是之前——放在之前只会把跳变挪到斜坡结束的那一帧。
    """
    off = np.zeros(max(0, int(n)), dtype=np.float64)
    r = max(1, int(ramp))
    for idx, d in steps:
        try:
            idx = int(idx)
            d = float(d)
        except (TypeError, ValueError):
            continue
        if not (0 <= idx < off.size) or not math.isfinite(d):
            continue
        d = min(max(d, -MAX_STEP_EV), MAX_STEP_EV)
        end = min(off.size, idx + r + 1)
        k = np.arange(idx, end, dtype=np.float64)
        off[idx:end] += -d * (1.0 - (k - idx) / r)
    return off


def _fmt_exp(t):
    try:
        t = float(t)
    except (TypeError, ValueError):
        return "?"
    if t >= 0.5:
        return f"{t:g}s"
    return f"1/{int(round(1.0 / t))}s"


def describe_step(prev, cur, d):
    """用前后两帧的 EXIF 生成「ISO 100→800 (+3.0EV)」这类短标签。"""
    d = float(d)
    if isinstance(prev, dict) and isinstance(cur, dict):
        i0, i1 = prev.get("iso"), cur.get("iso")
        if i0 and i1 and abs(math.log2(i1 / i0)) >= 0.2:
            return f"ISO {int(round(i0))}→{int(round(i1))} ({d:+.1f}EV)"
        e0, e1 = prev.get("exp"), cur.get("exp")
        if e0 and e1 and abs(math.log2(e1 / e0)) >= 0.2:
            return f"快门 {_fmt_exp(e0)}→{_fmt_exp(e1)} ({d:+.1f}EV)"
        f0, f1 = prev.get("f"), cur.get("f")
        if f0 and f1 and abs(math.log2(f1 / f0)) >= 0.2:
            return f"光圈 f/{f0:g}→f/{f1:g} ({d:+.1f}EV)"
    return f"{d:+.1f} EV"


def steps_with_labels(steps, infos):
    """[(帧号, 档差)] + 逐帧 EXIF -> [(帧号, 档差, 标签)]。"""
    out = []
    for idx, d in steps:
        prev = infos[idx - 1] if 0 < idx <= len(infos) else None
        cur = infos[idx] if 0 <= idx < len(infos) else None
        out.append((int(idx), float(d), describe_step(prev, cur, d)))
    return out
