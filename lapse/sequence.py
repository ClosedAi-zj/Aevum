# -*- coding: utf-8 -*-
"""序列关键帧：关键帧之间逐参数插值，得到每一帧的调色参数。

纯数据模块，不依赖 Qt。关键帧是 [{index, params}] 的有序列表，params 与
lapse.grade.DEFAULT 同构。插值按参数类型分派：标量线性、色相走最短弧、
曲线重采样后插值、蒙版按稳定 id 配对（只在单侧出现的蒙版用 amount 淡入淡出）。
"""
import bisect
import copy

import numpy as np

from lapse import grade as gc

MAX_KEYFRAMES = 2000
CURVE_SAMPLES = 17
# 绝对色相（0..360）：过 0/360 时走最短弧，350° → 10° 的中点应是 0° 而不是 180°
HUE_KEYS = ("grade_global_h", "grade_sh_h", "grade_mid_h", "grade_hi_h")


def _floats(v, n):
    out = []
    for i in range(n):
        try:
            out.append(float(v[i]))
        except (TypeError, ValueError, IndexError):
            out.append(0.0)
    return out


def _is_curve(v):
    return (isinstance(v, (list, tuple)) and len(v) >= 2
            and all(isinstance(q, (list, tuple)) and len(q) == 2 for q in v))


def _is_flat(v):
    return (isinstance(v, (list, tuple))
            and all(isinstance(x, (int, float)) for x in v))


def _curve_mid(a, b, t):
    """控制点数量不同的两条曲线：各自求 LUT，在 y 上线性插值，再采样回控制点。"""
    if t <= 0.0:
        return copy.deepcopy(a)
    if t >= 1.0:
        return copy.deepcopy(b)
    if [[int(x), int(y)] for x, y in a] == [[int(x), int(y)] for x, y in b]:
        return copy.deepcopy(a)
    la = gc.pchip_lut(a, 256)
    lb = gc.pchip_lut(b, 256)
    xs = np.linspace(0, 255, CURVE_SAMPLES)
    out = []
    for i, x in enumerate(xs):
        xi = int(round(float(x)))
        y = float(la[xi] + (lb[xi] - la[xi]) * t)
        out.append([xi, int(round(min(max(y, 0.0), 255.0)))])
    return out


def _hue_mid(a, b, t):
    d = (float(b) - float(a) + 180.0) % 360.0 - 180.0
    return (float(a) + d * t) % 360.0


def _mask_key(m, i):
    mid = m.get("id") if isinstance(m, dict) else None
    return str(mid) if mid else f"__{i}"


def _mask_mid(a, b, t):
    out = copy.deepcopy(a)
    for k in ("x1", "y1", "x2", "y2", "feather", "angle", "midpoint", "amount"):
        if k in a or k in b:
            va = float(a.get(k, 0.0) or 0.0)
            vb = float(b.get(k, va) or 0.0)
            out[k] = va + (vb - va) * t
    if bool(a.get("invert")) != bool(b.get("invert")):
        out["invert"] = 1 if t >= 0.5 else 0
    da = a.get("params") or {}
    db = b.get("params") or {}
    params = {}
    for k in set(da) | set(db):
        va = float(da.get(k, 0.0) or 0.0)
        vb = float(db.get(k, 0.0) or 0.0)
        params[k] = va + (vb - va) * t
    out["params"] = params
    return out


def _step_mid(a, b, ka, kb, t, key):
    """布尔开关按中点切换：几何/参数可以插值，add/subtract 不能。"""
    return a.get(key, ka) if t < 0.5 else b.get(key, kb)


def _comp_mid(a, b, t):
    out = copy.deepcopy(a)
    for k in ("x1", "y1", "x2", "y2", "feather", "angle", "midpoint"):
        if k in a or k in b:
            va = float(a.get(k, 0.0) or 0.0)
            vb = float(b.get(k, va) or 0.0)
            out[k] = va + (vb - va) * t
    if bool(a.get("invert")) != bool(b.get("invert")):
        out["invert"] = 1 if t >= 0.5 else 0
    out["blend"] = _step_mid(a, b, "add", "add", t, "blend")
    return out


def _group_mid(a, b, t):
    out = copy.deepcopy(a)
    for k in ("amount",):
        va = float(a.get(k, 100.0) or 0.0)
        vb = float(b.get(k, va) or 0.0)
        out[k] = va + (vb - va) * t
    if bool(a.get("invert")) != bool(b.get("invert")):
        out["invert"] = 1 if t >= 0.5 else 0
    ra, rb = a.get("range") or {}, b.get("range") or {}
    if ra or rb:
        out["range"] = {
            "lum_on": 1 if (ra.get("lum_on") if t < 0.5 else rb.get("lum_on")) else 0,
            "lum_min": _lerp(ra.get("lum_min", 0.0), rb.get("lum_min", 0.0), t),
            "lum_max": _lerp(ra.get("lum_max", 1.0), rb.get("lum_max", 1.0), t),
            "lum_feather": _lerp(ra.get("lum_feather", 0.1),
                                 rb.get("lum_feather", 0.1), t),
        }
    da = a.get("params") or {}
    db = b.get("params") or {}
    out["params"] = {k: _lerp(da.get(k, 0.0), db.get(k, 0.0), t)
                     for k in set(da) | set(db)}
    ca = list(a.get("components") or ())
    cb = list(b.get("components") or ())
    by_b = {_mask_key(c, j): c for j, c in enumerate(cb)}
    used, comps = set(), []
    for i, c0 in enumerate(ca):
        key = _mask_key(c0, i)
        c1 = by_b.get(key)
        if c1 is None:
            if t < 0.5:
                comps.append(copy.deepcopy(c0))
            continue
        used.add(key)
        comps.append(_comp_mid(c0, c1, t))
    if t >= 0.5:
        for j, c1 in enumerate(cb):
            if _mask_key(c1, j) not in used:
                comps.append(copy.deepcopy(c1))
    out["components"] = comps
    return out


def _lerp(a, b, t):
    try:
        va, vb = float(a or 0.0), float(b or 0.0)
    except (TypeError, ValueError):
        return a if t < 0.5 else b
    return va + (vb - va) * t


def _masks_mid(a, b, t):
    ma = list(a.get("masks") or ())
    mb = list(b.get("masks") or ())
    if not ma and not mb:
        return []
    grouped = any("components" in m for m in ma + mb)
    if grouped:
        ma = [gc.as_group(m) for m in ma]
        mb = [gc.as_group(m) for m in mb]
    by_b = {_mask_key(m, j): m for j, m in enumerate(mb)}
    used = set()
    out = []
    for i, m0 in enumerate(ma):
        key = _mask_key(m0, i)
        m1 = by_b.get(key)
        if m1 is None:
            m = copy.deepcopy(m0)
            m["amount"] = float(m0.get("amount", 100.0) or 0.0) * (1.0 - t)
            out.append(m)
            continue
        used.add(key)
        out.append(_group_mid(m0, m1, t) if grouped
                   else _mask_mid(m0, m1, t))
    for j, m1 in enumerate(mb):
        key = _mask_key(m1, j)
        if key in used:
            continue
        m = copy.deepcopy(m1)
        m["amount"] = float(m1.get("amount", 100.0) or 0.0) * t
        out.append(m)
    return out


def interpolate(a, b, t):
    """两份参数在 t∈[0,1] 之间的插值副本。"""
    if t <= 0.0:
        return copy.deepcopy(a)
    if t >= 1.0:
        return copy.deepcopy(b)
    out = {}
    for k, dv in gc.DEFAULT.items():
        va = a.get(k, dv)
        vb = b.get(k, dv)
        if k == "masks":
            out[k] = _masks_mid(a, b, t)
        elif k.startswith("curve_") and _is_curve(va) and _is_curve(vb):
            out[k] = _curve_mid(va, vb, t)
        elif _is_flat(va) and _is_flat(vb):
            n = max(len(va), len(vb))
            out[k] = [x + (y - x) * t
                      for x, y in zip(_floats(va, n), _floats(vb, n))]
        elif isinstance(va, (int, float)) and isinstance(vb, (int, float)):
            out[k] = (_hue_mid(va, vb, t) if k in HUE_KEYS
                      else float(va) + (float(vb) - float(va)) * t)
        else:
            out[k] = copy.deepcopy(va if t < 0.5 else vb)
    # 将来加入的新键不在 DEFAULT 里时也不能丢
    for k in (set(a) | set(b)) - set(gc.DEFAULT):
        va, vb = a.get(k), b.get(k)
        if isinstance(va, (int, float)) and isinstance(vb, (int, float)):
            out[k] = float(va) + (float(vb) - float(va)) * t
        else:
            out[k] = copy.deepcopy(va if t < 0.5 else vb)
    return out


def normalize(keyframes, count=None):
    """按帧号排序去重并裁掉越界项；同帧后写的覆盖先写的。"""
    table = {}
    for kf in keyframes or ():
        if not isinstance(kf, dict):
            continue
        try:
            idx = int(kf.get("index", -1))
        except (TypeError, ValueError):
            continue
        if idx < 0 or (count is not None and idx >= int(count)):
            continue
        p = kf.get("params")
        if isinstance(p, dict):
            table[idx] = copy.deepcopy(p)
    return [{"index": i, "params": table[i]} for i in sorted(table)][:MAX_KEYFRAMES]


def _index_of(kf):
    if not isinstance(kf, dict):
        return None
    try:
        return int(kf.get("index", -1))
    except (TypeError, ValueError):
        return None


def upsert(keyframes, index, params):
    """在 index 记录/更新一个关键帧，返回新的有序列表。"""
    idx = int(index)
    out = [copy.deepcopy(kf) for kf in keyframes or ()
           if _index_of(kf) is not None and _index_of(kf) != idx]
    out.append({"index": idx, "params": copy.deepcopy(params)})
    out.sort(key=lambda kf: int(kf["index"]))
    return out[:MAX_KEYFRAMES]


def remove(keyframes, index):
    idx = int(index)
    return [copy.deepcopy(kf) for kf in keyframes or ()
            if _index_of(kf) is not None and _index_of(kf) != idx]


def at(keyframes, index):
    idx = int(index)
    for kf in keyframes or ():
        if _index_of(kf) == idx:
            return kf
    return None


def indices(keyframes):
    return [i for i in (_index_of(kf) for kf in keyframes or ())
            if i is not None and i >= 0]


def params_at(base, keyframes, index):
    """第 index 帧的调色参数：范围外取最近关键帧，之间线性插值。"""
    kfs = sorted((kf for kf in keyframes or ()
                  if _index_of(kf) is not None
                  and isinstance(kf.get("params"), dict)),
                 key=_index_of)
    if not kfs:
        return base
    try:
        idx = int(index)
    except (TypeError, ValueError):
        idx = 0
    if idx <= int(kfs[0]["index"]):
        return copy.deepcopy(kfs[0]["params"])
    if idx >= int(kfs[-1]["index"]):
        return copy.deepcopy(kfs[-1]["params"])
    for lo, hi in zip(kfs, kfs[1:]):
        a, b = int(lo["index"]), int(hi["index"])
        if a <= idx <= b:
            if idx == a:
                return copy.deepcopy(lo["params"])
            if idx == b:
                return copy.deepcopy(hi["params"])
            return interpolate(lo["params"], hi["params"],
                               (idx - a) / float(b - a))
    return copy.deepcopy(kfs[-1]["params"])


def _kf_exposure(kf):
    try:
        return float((kf.get("params") or {}).get("exposure", 0.0))
    except (TypeError, ValueError):
        return 0.0


def exposure_track(base, keyframes, count):
    """逐帧曝光值，规则与 params_at(...)["exposure"] 一致，但只算这一个标量。

    画"校正后"的亮度走势要每帧的曝光；用 params_at 逐帧深拷贝整套参数（含蒙版）
    在几千帧的序列上太贵，这里按帧号二分，O(n log k)。
    """
    try:
        n = max(0, int(count))
    except (TypeError, ValueError):
        return []
    table = {}
    for kf in keyframes or ():
        i = _index_of(kf)
        if i is not None and isinstance(kf.get("params"), dict):
            table[i] = _kf_exposure(kf)
    if not table:
        try:
            fallback = float(base.get("exposure", 0.0)) if isinstance(base, dict) \
                else float(base)
        except (TypeError, ValueError):
            fallback = 0.0
        return [fallback] * n
    idxs = sorted(table)
    vals = [table[i] for i in idxs]
    out = []
    for i in range(n):
        j = bisect.bisect_left(idxs, i)
        if j <= 0:
            out.append(vals[0])
        elif j >= len(idxs):
            out.append(vals[-1])
        elif idxs[j] == i:
            out.append(vals[j])
        else:
            a, b = idxs[j - 1], idxs[j]
            va, vb = vals[j - 1], vals[j]
            out.append(va + (vb - va) * (i - a) / float(b - a))
    return out
