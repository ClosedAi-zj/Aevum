# -*- coding: utf-8 -*-
"""调色参数旁车文件：崩溃/重启后还能接着改。

放在 roaming 的 edits/，绝不放进 cache：清缓存只会删缩略图。
写入用临时文件 + os.replace，断电时要么是旧版本、要么是新版本。
读取时逐项校验，坏文件只当没有记录，不能拖垮启动。
"""
import hashlib
import json
import math
import os
import time

import numpy as np

from reference import grade as gc
from reference.gui import prefs

VERSION = 1
MAX_MASKS = 32
MAX_KEYFRAMES = 2000


def enabled():
    return prefs.as_bool(prefs.get("edits/autosave", True), True)


def edits_dir():
    raw = str(prefs.get("edits/dir", "") or "").strip()
    return os.path.normpath(os.path.expanduser(raw)) if raw \
        else os.path.join(prefs._roaming_root(), "edits")


def _norm(key):
    kind, _, path = str(key).partition(":")
    if not path:
        kind, path = "file", str(key)
    return kind + ":" + os.path.normcase(os.path.normpath(path))


def _hash(key):
    return hashlib.sha1(_norm(key).encode("utf-8")).hexdigest()[:24]


def record_path(key):
    return os.path.join(edits_dir(), _hash(key) + ".json")


def deflicker_path(key):
    return os.path.join(edits_dir(), _hash(key) + ".deflicker.json")


def write_json(path, payload):
    """原子写入；断电时要么是旧版本、要么是新版本，不会留半截 JSON。

    导出进度一类的旁车文件也用这一份，别在调用方再写一套 tmp/fsync。
    """
    try:
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        tmp = path + ".tmp"
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(payload, f, ensure_ascii=False, separators=(",", ":"))
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
        return True
    except (OSError, TypeError, ValueError):
        return False


def save(key, p):
    """原子写入；任何 IO 异常都只返回 False，自动保存不能打断调色。"""
    return write_json(record_path(key), {
        "version": VERSION, "key": _norm(key),
        "saved": time.strftime("%Y-%m-%d %H:%M:%S"),
        "params": _jsonable(p)})


def load(key):
    """返回 sanitize 过的参数；没有记录/坏记录返回 None。"""
    try:
        path = record_path(key)
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
        if not isinstance(data, dict) or int(data.get("version", 0)) > VERSION:
            return None
        if _norm(data.get("key", "")) != _norm(key):
            return None
        raw = data.get("params")
        return sanitize(raw) if isinstance(raw, dict) else None
    except (OSError, ValueError, TypeError):
        return None


def clear(key):
    """删掉应用自己写的旁车文件；素材文件永远不碰。"""
    try:
        os.remove(record_path(key))
    except OSError:
        pass


# ---------------------------------------------------------------- 去闪烁
MAX_FLICKER_EV = 3.0
MAX_FRAMES = 200000


def _flicker_files(files):
    out = []
    if isinstance(files, (list, tuple)):
        for f in files[:MAX_FRAMES]:
            out.append(os.path.basename(str(f)))
    return out


def _flicker_chan(*raws):
    """旁车里到底是每帧一个数还是每帧三个数，看数据自己长什么样。

    老记录是纯标量，读出来就是 False；不用版本号也能兼容。
    """
    for raw in raws:
        if isinstance(raw, (list, tuple)):
            for v in raw:
                if v is not None:
                    return isinstance(v, (list, tuple))
    return False


def _flicker_offsets(raw, n, chan=False):
    out = []
    vals = raw if isinstance(raw, (list, tuple)) else []
    for i in range(min(int(n), MAX_FRAMES)):
        if i >= len(vals):
            out.append([0.0] * 3 if chan else 0.0)
        elif chan:
            row = vals[i] if isinstance(vals[i], (list, tuple)) else []
            out.append([_clamped(row[c] if c < len(row) else 0.0,
                                 -MAX_FLICKER_EV, MAX_FLICKER_EV)
                        for c in range(3)])
        else:
            out.append(_clamped(vals[i], -MAX_FLICKER_EV, MAX_FLICKER_EV))
    return out


def _flicker_means(raw, n, chan=False):
    out = []
    vals = raw if isinstance(raw, (list, tuple)) else []
    for i in range(min(int(n), MAX_FRAMES)):
        if not chan:
            v = _num(vals[i], 0.0) if i < len(vals) else 0.0
            out.append(v if v > 0.0 else None)
            continue
        row = vals[i] if i < len(vals) and isinstance(vals[i], (list, tuple)) \
            else []
        item = []
        for c in range(3):
            v = _num(row[c], 0.0) if c < len(row) else 0.0
            item.append(v if v > 0.0 else None)
        out.append(item)
    return out


def save_deflicker(key, data):
    """整段序列的测光值与逐帧曝光偏移，和调色参数分开存。

    分通道时 means/offsets 每帧是 [R,G,B] 三个数，另外记下 channels/multi，
    重开软件能按同样的设置复现。
    """
    if not isinstance(data, dict):
        return False
    files = _flicker_files(data.get("files"))
    n = len(files)
    if n < 2:
        return False
    chan = _flicker_chan(data.get("offsets"), data.get("means"))
    payload = {
        "version": VERSION, "key": _norm(key),
        "saved": time.strftime("%Y-%m-%d %H:%M:%S"),
        "window": int(_clamped(data.get("window"), 3, 199, 15)),
        "strength": int(_clamped(data.get("strength"), 0, 100, 100)),
        "channels": bool(chan),
        "multi": bool(data.get("multi")) if chan else False,
        "files": files,
        "means": _flicker_means(data.get("means"), n, chan),
        "offsets": _flicker_offsets(data.get("offsets"), n, chan),
    }
    return write_json(deflicker_path(key), payload)


def load_deflicker(key, files):
    """读取并核对帧名单；序列变了（增删/重命名）就只当没有记录。"""
    names = _flicker_files(files)
    if len(names) < 2:
        return None
    try:
        with open(deflicker_path(key), "r", encoding="utf-8") as f:
            data = json.load(f)
        if not isinstance(data, dict) or int(data.get("version", 0)) > VERSION:
            return None
        if _norm(data.get("key", "")) != _norm(key):
            return None
        saved = _flicker_files(data.get("files"))
        if saved != names:
            return None
        n = len(names)
        chan = _flicker_chan(data.get("offsets"), data.get("means"))
        means = _flicker_means(data.get("means"), n, chan)
        offsets = _flicker_offsets(data.get("offsets"), n, chan)
        return {
            "window": int(_clamped(data.get("window"), 3, 199, 15)),
            "strength": int(_clamped(data.get("strength"), 0, 100, 100)),
            "channels": bool(chan),
            "multi": bool(data.get("multi")) if chan else False,
            "files": names, "means": means, "offsets": offsets,
        }
    except (OSError, ValueError, TypeError):
        return None


def clear_deflicker(key):
    try:
        os.remove(deflicker_path(key))
    except OSError:
        pass


# ---------------------------------------------------------------- 曝光台阶
MAX_HOLY_EV = 20.0
MAX_HOLY_RAMP = 60


def holy_path(key):
    return os.path.join(edits_dir(), _hash(key) + ".holy.json")


def _holy_steps(raw, n):
    """[(帧号, 档差, 标签)]；坏项丢掉，同帧后写的覆盖先写的。"""
    table = {}
    for item in raw if isinstance(raw, (list, tuple)) else ():
        if isinstance(item, dict):
            i, d, label = item.get("index"), item.get("delta"), item.get("label")
        elif isinstance(item, (list, tuple)) and len(item) >= 2:
            i, d = item[0], item[1]
            label = item[2] if len(item) > 2 else ""
        else:
            continue
        try:
            i = int(i)
            d = float(d)
        except (TypeError, ValueError):
            continue
        if not (0 < i < n) or not math.isfinite(d) or abs(d) < 0.01:
            continue
        table[i] = [i, round(min(max(d, -MAX_HOLY_EV), MAX_HOLY_EV), 3),
                    str(label or "")[:80]]
    return [table[i] for i in sorted(table)]


def _holy_offsets(raw, n):
    out = []
    vals = raw if isinstance(raw, (list, tuple)) else []
    for i in range(min(int(n), MAX_FRAMES)):
        out.append(_clamped(vals[i], -MAX_HOLY_EV, MAX_HOLY_EV)
                   if i < len(vals) else 0.0)
    return out


def save_holy(key, data):
    """台阶位置、来源与逐帧补偿分开存；补偿可随时由台阶和 ramp 重算。"""
    if not isinstance(data, dict):
        return False
    files = _flicker_files(data.get("files"))
    n = len(files)
    steps = _holy_steps(data.get("steps"), n)
    if n < 2 or not steps:
        return False
    if data.get("source") not in ("exif", "luma"):
        source = "exif"
    else:
        source = data["source"]
    return write_json(holy_path(key), {
        "version": VERSION, "key": _norm(key),
        "saved": time.strftime("%Y-%m-%d %H:%M:%S"),
        "source": source,
        "ramp": int(_clamped(data.get("ramp"), 2, MAX_HOLY_RAMP, 12)),
        "files": files, "steps": steps,
        "offsets": _holy_offsets(data.get("offsets"), n),
    })


def load_holy(key, files):
    names = _flicker_files(files)
    if len(names) < 2:
        return None
    try:
        with open(holy_path(key), "r", encoding="utf-8") as f:
            data = json.load(f)
        if not isinstance(data, dict) or int(data.get("version", 0)) > VERSION:
            return None
        if _norm(data.get("key", "")) != _norm(key):
            return None
        if _flicker_files(data.get("files")) != names:
            return None
        steps = _holy_steps(data.get("steps"), len(names))
        if not steps:
            return None
        source = data.get("source")
        return {
            "files": names,
            "source": source if source in ("exif", "luma") else "exif",
            "ramp": int(_clamped(data.get("ramp"), 2, MAX_HOLY_RAMP, 12)),
            "steps": steps,
            "offsets": _holy_offsets(data.get("offsets"), len(names)),
        }
    except (OSError, ValueError, TypeError):
        return None


def clear_holy(key):
    try:
        os.remove(holy_path(key))
    except OSError:
        pass


# ---------------------------------------------------------------- 序列关键帧
def keyframes_path(key):
    return os.path.join(edits_dir(), _hash(key) + ".keyframes.json")


def _keyframe_list(raw, n):
    """按帧号去重排序，逐项 sanitize；越界或坏项直接丢掉。"""
    table = {}
    for kf in raw if isinstance(raw, (list, tuple)) else ():
        if not isinstance(kf, dict):
            continue
        try:
            i = int(kf.get("index", -1))
        except (TypeError, ValueError):
            continue
        p = kf.get("params")
        if 0 <= i < n and isinstance(p, dict):
            table[i] = sanitize(p)      # 同帧后写的覆盖先写的
    return [{"index": i, "params": table[i]}
            for i in sorted(table)][:MAX_KEYFRAMES]


def save_keyframes(key, data):
    """关键帧与基准参数分开存：开关、帧名单、逐帧参数快照。"""
    if not isinstance(data, dict):
        return False
    files = _flicker_files(data.get("files"))
    if len(files) < 2:
        return False
    frames = _keyframe_list(data.get("keyframes"), len(files))
    if not frames:
        return False
    return write_json(keyframes_path(key), {
        "version": VERSION, "key": _norm(key),
        "saved": time.strftime("%Y-%m-%d %H:%M:%S"),
        "on": bool(data.get("on", True)),
        "files": files, "keyframes": frames})


def load_keyframes(key, files):
    """读取并核对帧名单；序列变了就只当没有关键帧。"""
    names = _flicker_files(files)
    if len(names) < 2:
        return None
    try:
        with open(keyframes_path(key), "r", encoding="utf-8") as f:
            data = json.load(f)
        if not isinstance(data, dict) or int(data.get("version", 0)) > VERSION:
            return None
        if _norm(data.get("key", "")) != _norm(key):
            return None
        if _flicker_files(data.get("files")) != names:
            return None
        frames = _keyframe_list(data.get("keyframes"), len(names))
        if not frames:
            return None
        return {"files": names, "on": bool(data.get("on", True)),
                "keyframes": frames}
    except (OSError, ValueError, TypeError):
        return None


def clear_keyframes(key):
    try:
        os.remove(keyframes_path(key))
    except OSError:
        pass


def _jsonable(v):
    if isinstance(v, dict):
        return {str(k): _jsonable(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [_jsonable(x) for x in v]
    if isinstance(v, np.ndarray):
        return _jsonable(v.tolist())
    if isinstance(v, np.generic):
        v = v.item()
    if isinstance(v, float) and not math.isfinite(v):
        return 0.0
    if isinstance(v, (str, int, float, bool)) or v is None:
        return v
    return 0.0


def _num(v, default=0.0):
    try:
        f = float(v)
    except (TypeError, ValueError):
        return float(default)
    return f if math.isfinite(f) else float(default)


def _clamped(v, lo, hi, default=0.0):
    return float(min(max(_num(v, default), lo), hi))


def _band(v, n=8):
    if not isinstance(v, (list, tuple)) or len(v) != n:
        return [0.0] * n
    return [_num(x) for x in v]


def _curve(v, default):
    if not isinstance(v, (list, tuple)) or len(v) < 2:
        return default
    pts = []
    for q in v:
        if not isinstance(q, (list, tuple)) or len(q) != 2:
            return default
        x = int(round(min(max(_num(q[0]), 0.0), 255.0)))
        y = int(round(min(max(_num(q[1]), 0.0), 255.0)))
        if pts and x <= pts[-1][0]:
            continue
        pts.append([x, y])
    return pts if len(pts) >= 2 else default


def _mask_id(v):
    s = str(v or "").strip()
    return s if 0 < len(s) <= 64 and all(c.isalnum() or c in "-_." for c in s) else ""


MAX_COMPONENTS = 16
_BLENDS = ("add", "subtract", "intersect")


def _range(raw):
    if not isinstance(raw, dict):
        return {}
    return {"lum_on": 1 if raw.get("lum_on") else 0,
            "lum_min": _clamped(raw.get("lum_min"), 0.0, 1.0, 0.0),
            "lum_max": _clamped(raw.get("lum_max"), 0.0, 1.0, 1.0),
            "lum_feather": _clamped(raw.get("lum_feather"), 0.001, 1.0, 0.1)}


def _component(c):
    kind = c.get("kind") or c.get("type") if isinstance(c, dict) else None
    if kind not in ("linear", "radial"):
        return None
    out = gc.new_component(kind)
    out["id"] = _mask_id(c.get("id")) or out["id"]
    for k in ("x1", "y1", "x2", "y2"):
        out[k] = _clamped(c.get(k), 0.0, 1.0, out[k])
    out["feather"] = _clamped(c.get("feather"), 0.0, 100.0, out["feather"])
    out["invert"] = 1 if c.get("invert") else 0
    out["angle"] = _clamped(c.get("angle"), -180.0, 180.0, 0.0)
    out["midpoint"] = _clamped(c.get("midpoint"), 0.0, 100.0, 50.0)
    out["blend"] = c.get("blend") if c.get("blend") in _BLENDS else "add"
    rm = _range(c.get("range"))
    if rm:
        out["range"] = rm
    return out


def _params(raw):
    if not isinstance(raw, dict):
        return {}
    keys = gc.MASK_KEYS + gc.MASK_SPATIAL
    return {k: _clamped(raw[k], -1000.0, 1000.0) for k in keys if k in raw}


def _mask(m):
    """读旁车蒙版：老的单形状格式在这里就地升级成单组件组。"""
    if not isinstance(m, dict):
        return None
    if "components" in m:
        comps = [_component(c) for c in m.get("components") or ()]
        comps = [c for c in comps if c is not None][:MAX_COMPONENTS]
        if not comps:
            return None
        g = gc.new_group("linear")
        g["components"] = comps
        g["id"] = _mask_id(m.get("id")) or g["id"]
    elif m.get("type") in ("linear", "radial"):
        g = gc.as_group(m)
        c = _component(g["components"][0])
        if c is None:
            return None
        g["components"] = [c]
    else:
        return None
    g["params"] = _params(m.get("params"))
    g["amount"] = _clamped(m.get("amount"), 0.0, 100.0, 100.0)
    # 老格式的 invert 在组件上（先反选再乘强度），组这里不能再反一次
    g["invert"] = 0 if "components" not in m else (1 if m.get("invert") else 0)
    rm = _range(m.get("range"))
    if rm:
        g["range"] = rm
    else:
        g.pop("range", None)
    return g


def sanitize(raw):
    """只接受已知字段；类型/长度不对就落回默认值。"""
    p = gc.defaults()
    for k, dv in gc.DEFAULT.items():
        if k not in raw:
            continue
        v = raw[k]
        if k == "masks":
            ms = [_mask(m) for m in v] if isinstance(v, list) else []
            p[k] = [m for m in ms if m is not None][:MAX_MASKS]
        elif k.startswith("curve_"):
            p[k] = _curve(v, dv)
        elif isinstance(dv, list) and len(dv) == 8:
            p[k] = _band(v)
        else:
            p[k] = _num(v, dv)
    return p
