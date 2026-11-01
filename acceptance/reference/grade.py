# -*- coding: utf-8 -*-
"""调色核心算法（无 GUI）。

参数风格参考 Photoshop Camera Raw / Lightroom / Capture One。
在 float32 [0,1] RGB 上运算；for_lut=True 时跳过空间效果（清晰度/晕影），
用于生成 3D LUT（.cube），保证视频导出与预览一致。
"""
import collections
import copy
import math
import threading
import uuid

import numpy as np
import cv2

LUMA = np.array([0.2126, 0.7152, 0.0722], dtype=np.float32)

# 多尺度降噪的 eps 常数，与 Rust `develop/spatial.rs::NR_MS_EPS_C` / `NR_MS_EPS_MIN`
# 同值：eps = C·σ̂²/rm，下限 NR_MS_EPS_MIN 只兜除零。C 为什么是 0.8、1/rm 为什么
# 成立、三画幅扫参表，全部写在 Rust 那两个常量的文档注释里（单一出处，别在这里
# 再抄一份表，抄了就会漂）。
NR_MS_EPS_C = np.float32(0.8)
NR_MS_EPS_MIN = np.float32(1e-6)

HSL_NAMES = ("红", "橙", "黄", "绿", "浅绿", "蓝", "紫", "洋红")
HSL_CENTERS = np.array([0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 285.0, 330.0],
                       dtype=np.float32)

DEFAULT = {
    "exposure": 0.0,
    # LR 对齐（下一轮设计 2026-10-30）：默认 0 = 旧数值路径逐位一致
    "hl_rolloff": 0.0,
    "contrast": 0.0, "highlights": 0.0, "shadows": 0.0,
    "whites": 0.0, "blacks": 0.0,
    "black_soft": 0.0, "white_soft": 0.0,
    "contrast_linear": 0.0,
    "temperature": 0.0, "tint": 0.0,
    "vibrance": 0.0, "saturation": 0.0,
    "clarity": 0.0, "texture": 0.0, "dehaze": 0.0, "grain": 0.0,
    # 暗通道去雾（设计 2026-10-30 §2）：dark 0 = 旧逐点路径逐位一致；
    # contrast 仅在 dark>0 时生效（幅度 contrast·0.15，需拟合）
    "dehaze_dark": 0.0, "dehaze_contrast": 0.0,
    "grain_size": 1.0, "grain_freq": 1.0,
    "grain_lum": 0.0,
    # 晕影：amount 仍叫 vignette（兼容旧档），另加四个收边参数
    "vignette": 0.0, "vig_midpoint": 50.0, "vig_roundness": 0.0,
    "vig_feather": 50.0, "vig_hl_contrast": 0.0,
    # 细节：锐化（USM + 边缘蒙版 + halo 限制）与亮度/色度降噪
    "sharp_amount": 0.0, "sharp_radius": 1.0, "sharp_detail": 25.0,
    "sharp_masking": 0.0, "sharp_protect": 0.0,
    "nr_luma": 0.0, "nr_color": 0.0,
    "nr_luma_detail": 0.0, "nr_luma_contrast": 0.0, "nr_color_smooth": 50.0,
    # 降噪多尺度 + 手动 σ（设计 2026-10-30 §1）：全 0 = 旧单尺度逐位一致
    "nr_multiscale": 0.0, "nr_sigma_manual": 0.0,
    # 色彩分级：全局轮 + 阴影 / 中间调 / 高光三段，各带 Lum 与过渡
    "grade_global_h": 0.0, "grade_global_s": 0.0, "grade_global_l": 0.0,
    "grade_sh_h": 0.0, "grade_sh_s": 0.0, "grade_sh_l": 0.0,
    "grade_mid_h": 0.0, "grade_mid_s": 0.0, "grade_mid_l": 0.0,
    "grade_hi_h": 0.0, "grade_hi_s": 0.0, "grade_hi_l": 0.0,
    "grade_balance": 0.0, "grade_blending": 0.0,
    # 色阶：输入黑白场 + 中间调 gamma + 输出黑白场（与曲线合成同一张表）
    "lv_black": 0.0, "lv_white": 255.0, "lv_gamma": 1.0,
    "lv_out_black": 0.0, "lv_out_white": 255.0,
    # 参数曲线：4 个亮度区 + 3 个分割点（Lightroom 的 Parametric 模式）
    "par_shadows": 0.0, "par_darks": 0.0,
    "par_lights": 0.0, "par_highlights": 0.0,
    "par_split1": 25.0, "par_split2": 50.0, "par_split3": 75.0,
    # 黑白：单色开关 + 8 个色相带的亮度混合（Lightroom 的 B&W Mixer）
    "mono": 0.0, "mono_h": [0.0] * 8,
    # 镜头校正：几何畸变、暗角补偿、去紫边（空间运算，不进 LUT）
    "lens_distort": 0.0, "lens_vignette": 0.0, "lens_fringe": 0.0,
    "hsl_h": [0.0] * 8, "hsl_s": [0.0] * 8, "hsl_l": [0.0] * 8,
    # 局部蒙版：每张一张渐变（线性/径向），params 里是相对全局参数的增量
    "masks": [],
    "curve_rgb": [[0, 0], [255, 255]],
    "curve_r": [[0, 0], [255, 255]],
    "curve_g": [[0, 0], [255, 255]],
    "curve_b": [[0, 0], [255, 255]],
    # 曲线 HDR 尾部：x>1 按末端斜率延伸而非硬截断（LR LumaHDR 口径）
    "curve_hdr_tail": 0.0,
}

PRESETS = {
    "原始": {},
    "鲜艳风光": {"vibrance": 35, "saturation": 10, "clarity": 15, "contrast": 12,
                 "texture": 8, "dehaze": 12, "sharp_amount": 40,
                 "sharp_masking": 25},
    "落日暖调": {"temperature": 28, "tint": 6, "vibrance": 22,
                 "highlights": -20, "shadows": 10, "clarity": 10,
                 "grade_hi_h": 40, "grade_hi_s": 18},
    "冷色电影": {"temperature": -22, "tint": -6, "contrast": 16,
                 "saturation": -8, "shadows": 12,
                 "grade_sh_h": 215, "grade_sh_s": 28,
                 "grade_hi_h": 35, "grade_hi_s": 12,
                 "curve_rgb": [[0, 18], [64, 58], [192, 206], [255, 254]]},
    "复古胶片": {"temperature": 4, "saturation": -24, "contrast": -8,
                 "blacks": -8,
                 "vignette": 30, "grain": 18,
                 "grade_sh_h": 150, "grade_sh_s": 22,
                 "grade_hi_h": 200, "grade_hi_s": 10,
                 "curve_rgb": [[0, 24], [64, 70], [192, 215], [255, 248]]},
    "黑白对比": {"mono": 100, "contrast": 28, "clarity": 22,
                 "blacks": -15, "sharp_amount": 35, "sharp_masking": 40},
    "夜景增强": {"exposure": 0.45, "shadows": 32, "highlights": -25,
                 "vibrance": 18, "temperature": -10, "clarity": 8,
                 "nr_luma": 30, "nr_color": 45, "sharp_masking": 50},
    "通透风光": {"contrast": 10, "highlights": -22, "shadows": 14,
                 "vibrance": 28, "dehaze": 18, "clarity": 18,
                 "texture": 10, "sharp_amount": 45, "sharp_masking": 20},
    "柔和人像": {"highlights": -16, "shadows": 20, "vibrance": 12,
                 "saturation": -4, "texture": -25, "clarity": -10,
                 "nr_luma": 25, "nr_color": 30,
                 "sharp_amount": 30, "sharp_masking": 60},
    "青橙电影": {"temperature": -8, "contrast": 14, "shadows": 8,
                 "saturation": -5, "clarity": 10,
                 "grade_sh_h": 210, "grade_sh_s": 35,
                 "grade_hi_h": 35, "grade_hi_s": 28},
}


def defaults():
    import copy
    return copy.deepcopy(DEFAULT)


# 局部蒙版能携带的参数：影调与颜色类，都是"相对全局的增量"。
# 细节（降噪/锐化）、全局空间校正（镜头）和蒙版本身不在其中——
# 局部锐化要在蒙版里再跑一遍全分辨率 USM，预览会慢到没法用。
MASK_KEYS = ("exposure", "temperature", "tint", "contrast", "highlights",
             "shadows", "whites", "blacks", "dehaze", "vibrance",
             "saturation", "clarity", "texture")
# 局部空间项：不在逐点增量里叠加，合成后按组权重单遍执行
MASK_SPATIAL = ("sharp_amount", "nr_luma", "nr_color",
                "sharp_radius", "sharp_detail", "sharp_masking",
                "nr_luma_detail", "nr_luma_contrast", "nr_color_smooth")

# 新建蒙版的默认几何：归一化坐标，与图像尺寸无关。
# angle/midpoint/amount 与 Lightroom 局部调整的语义一致：角度转方向，
# 中点挪 50% 过渡线，强度按比例削弱整块效果。
MASK_NEW = {
    "linear": {"type": "linear", "x1": 0.5, "y1": 0.30,
               "x2": 0.5, "y2": 0.62, "feather": 45, "invert": 0,
               "angle": 0.0, "midpoint": 50.0, "amount": 100.0},
    "radial": {"type": "radial", "x1": 0.5, "y1": 0.50,
               "x2": 0.72, "y2": 0.50, "feather": 35, "invert": 0,
               "angle": 0.0, "midpoint": 50.0, "amount": 100.0},
}


def new_mask(kind):
    """旧式扁平蒙版（一个蒙版=一个形状），保留给旧档和外部脚本。"""
    m = dict(MASK_NEW[kind])
    # 稳定 id：关键帧插值靠它把两张蒙版配成对，位置/顺序变了也不会错配
    m["id"] = uuid.uuid4().hex[:12]
    m["params"] = {}
    return m


def new_component(kind):
    """组内形状：只带几何，不带调色参数（参数在组上，与 LR 一致）。"""
    c = dict(MASK_NEW[kind])
    c.pop("type", None)
    c["kind"] = kind
    c["id"] = uuid.uuid4().hex[:12]
    c["blend"] = "add"              # add | subtract | intersect
    return c


def new_group(kind):
    """一个局部调整组 = 多个形状布尔组合 + 一套增量 + 整体强度。"""
    return {"id": uuid.uuid4().hex[:12],
            "components": [new_component(kind)],
            "params": {}, "amount": 100.0, "invert": 0}


def as_group(m):
    """扁平蒙版包成单组件组；几何与强度语义原样保留。"""
    if not isinstance(m, dict) or "components" in m:
        return m
    c = {k: copy.deepcopy(v) for k, v in m.items()
         if k not in ("params", "amount", "type", "range")}
    c["kind"] = m.get("type", "linear")
    c.setdefault("id", uuid.uuid4().hex[:12])
    c["blend"] = "add"
    return {"id": m.get("id") or uuid.uuid4().hex[:12],
            "components": [c],
            "params": copy.deepcopy(m.get("params") or {}),
            "amount": float(m.get("amount", 100.0) or 0.0),
            "invert": 0,
            "range": copy.deepcopy(m.get("range") or {})}


def _kind(c):
    return c.get("kind") or c.get("type") or "linear"


def _mask_delta(p, d):
    """把一张蒙版的增量叠到全局参数上，得到「只在这块区域生效」的参数副本。"""
    q = dict(p)
    for k, v in d.items():
        base = p.get(k)
        if base is None or k not in MASK_KEYS:
            continue
        q[k] = float(base) + float(v)
    return q


def _mask_active(p):
    """真正会改变画面的蒙版（参数全为 0 或强度为 0 的只占位）。"""
    out = []
    for m in p.get("masks") or ():
        if float(m.get("amount", 100.0) or 0.0) <= 1e-6:
            continue
        if not _any_delta(m.get("params") or {}):
            continue
        if "components" in m and not (m.get("components") or ()):
            continue
        out.append(m)
    return out


def changes(p):
    """与默认值是否有差异（曲线是否被改动）。只比较用户可调项。"""
    if _mask_active(p):
        return True
    for k, v in DEFAULT.items():
        if k == "masks":
            continue
        cur = p.get(k, v)
        if isinstance(v, list) and v and isinstance(v[0], (int, float)):
            if len(cur) != len(v) or any(abs(a - b) > 1e-6
                                         for a, b in zip(cur, v)):
                return True
        elif isinstance(v, dict):
            if any(abs(cur.get(k2, 0) - v2) > 1e-6 for k2, v2 in v.items()):
                return True
        elif isinstance(v, list):                       # 曲线点列
            if [[int(x), int(y)] for x, y in cur] != [[int(x), int(y)] for x, y in v]:
                return True
        elif abs(float(cur) - float(v)) > 1e-6:
            return True
    return False


def preset(name):
    p = defaults()
    for k, v in PRESETS[name].items():
        p[k] = copy.deepcopy(v) if isinstance(v, list) else v
    return p


def _smooth(e0, e1, x):
    t = np.clip((x - e0) / (e1 - e0), 0.0, 1.0)
    return t * t * (3.0 - 2.0 * t)


def _soft_clip_high(x, s):
    """白场软拐点（抛物线肩部）：f(1-s)=1-s、f'(1-s)=1、f(1+s)=1、f'(1+s)=0，
    C1 连续且恒 ≤1。与 Rust develop::soft_clip_high 逐式对应。"""
    dd = x - (1.0 - s)
    mid = (x > 1.0 - s) & (x < 1.0 + s)
    return np.where(x >= 1.0 + s, np.float32(1.0),
                    np.where(mid, x - dd * dd / np.float32(4.0 * s), x))


def _soft_clip_low(x, s):
    """黑场软拐点：与 _soft_clip_high 镜像，恒 ≥0。"""
    dd = s - x
    mid = (x < s) & (x > -s)
    return np.where(x <= -s, np.float32(0.0),
                    np.where(mid, x + dd * dd / np.float32(4.0 * s), x))


def pchip_lut(pts, n=256):
    """单调三次插值（Fritsch-Carlson），控制点 -> n 级 LUT（0..255）。"""
    pts = sorted((list(map(float, p)) for p in pts), key=lambda p: p[0])
    xs = np.asarray([p[0] for p in pts], dtype=np.float64)
    ys = np.asarray([p[1] for p in pts], dtype=np.float64)
    if len(xs) < 2:
        return np.linspace(0.0, 255.0, int(n), dtype=np.float32)
    h = np.diff(xs)
    d = np.diff(ys) / np.maximum(h, 1e-9)
    m = np.zeros_like(ys)
    m[0], m[-1] = d[0], d[-1]
    for i in range(1, len(xs) - 1):
        if d[i - 1] * d[i] <= 0:
            m[i] = 0.0
        else:
            w1 = 2 * h[i] + h[i - 1]
            w2 = h[i] + 2 * h[i - 1]
            m[i] = (w1 + w2) / (w1 / d[i - 1] + w2 / d[i])
    g = np.linspace(0.0, 255.0, int(n), dtype=np.float64)
    i = np.clip(np.searchsorted(xs, g, side="right") - 1, 0, len(xs) - 2)
    t = np.clip((g - xs[i]) / np.maximum(h[i], 1e-9), 0.0, 1.0)
    t2, t3 = t * t, t * t * t
    lut = ((2 * t3 - 3 * t2 + 1) * ys[i] + (t3 - 2 * t2 + t) * h[i] * m[i]
           + (-2 * t3 + 3 * t2) * ys[i + 1] + (t3 - t2) * h[i] * m[i + 1])
    return np.clip(lut, 0, 255).astype(np.float32)


def _curves_default(p):
    ident = [[0, 0], [255, 255]]
    return all([list(pt) for pt in p[k]] == ident
               for k in ("curve_rgb", "curve_r", "curve_g", "curve_b"))


def _vignette_falloff(H, W, roundness=0.0):
    """径向场 (H,W)：0=中心，1=四角，按 roundness 混合形状。

    roundness 0 = 跟随画幅的椭圆（16:9 上横向更扁）；100 = 像素空间的正圆。
    两张基础场按尺寸缓存，混合只是一次乘加。
    """
    key = (H, W)
    with _GEOM.lock:
        g = _GEOM.get(key)
        if g is None:
            _radius(H, W)
            g = _GEOM.get(key)
        rc = g.get("r_circ")
        if rc is None:
            sx = (np.arange(W, dtype=np.float32) - (W - 1) / 2) / (W / 2)
            sy = (np.arange(H, dtype=np.float32) - (H - 1) / 2) / (H / 2)
            half = max(np.hypot(W / 2.0, H / 2.0), 1e-6)
            rc = (np.sqrt((sx[None, :] * (W / 2.0)) ** 2
                          + (sy[:, None] * (H / 2.0)) ** 2) / half
                  ).astype(np.float32)
            g["r_circ"] = rc
            _GEOM.resize(key)
        rd = float(np.clip(roundness, 0.0, 100.0)) / 100.0
        if not rd:
            return g["r"]
        return g["r"] * (1.0 - rd) + rc * rd


def vignette_mask(W, H, p, lum=None):
    """裁剪后暗角乘法蒙版 (H,W)。正 amount = 压暗四角，负 = 提亮。

    amount 取 vig_amount，缺省回落到旧的 vignette 键；p 也可以直接传数值。
    midpoint 挪过渡起点，roundness 把椭圆拉向正圆，feather 控过渡宽度，
    hl_contrast 按像素亮度把亮部压暗量还回去（高光保护）。
    默认 50/50 精确复现旧的 _smooth(0.45, 1.05, r)，旧档与预设不变。
    """
    if not isinstance(p, dict):
        p = {"vig_amount": p}
    a = float(p.get("vig_amount", p.get("vignette", 0.0)) or 0.0) / 100.0
    if not a:
        return None
    mp_v = p.get("vig_midpoint")
    mp = float(np.clip(50.0 if mp_v is None else mp_v, 0.0, 100.0)) / 100.0
    fe_v = p.get("vig_feather")
    fe = float(np.clip(50.0 if fe_v is None else fe_v, 0.0, 100.0)) / 100.0
    hc = float(np.clip(p.get("vig_hl_contrast", 0.0) or 0.0,
                       -100.0, 100.0)) / 100.0
    r = _vignette_falloff(H, W, p.get("vig_roundness", 0.0))
    start = mp * 0.9
    width = max(fe * 1.2, 0.05)
    t = np.clip((r - start) / width, 0.0, 1.0)
    s = t * t * (3.0 - 2.0 * t)
    m = 1.0 - a * s
    if hc and lum is not None:
        bright = np.clip(np.asarray(lum, np.float32) * 2.0 - 1.0, 0.0, 1.0)
        m = m + hc * bright * (1.0 - m)
    return np.clip(m, 0.0, 2.0).astype(np.float32)


# ---- 与图像尺寸绑定的常量场：归一化半径、颗粒噪声 ----
# 每次渲染都重算的话，光 mgrid 与 standard_normal 就各要 20~140ms，
# 而它们只跟宽高有关。按字节预算做 LRU，而不是按条数：4K 与 900px 代理
# 的一张大 20 倍，按"两张"淘汰会让内存随素材尺寸悄悄膨胀。
def _nbytes(v):
    if isinstance(v, np.ndarray):
        return int(v.nbytes)
    if isinstance(v, dict):
        return sum(_nbytes(x) for x in v.values())
    if isinstance(v, (list, tuple)):
        return sum(_nbytes(x) for x in v)
    return 0


class _BudgetCache:
    """线程安全的 LRU 缓存，按数组实际字节数淘汰。

    渲染线程与导出线程可能同时取同一张常量场；用 RLock 是因为
    _vignette_falloff -> _radius 这类调用会重入。
    """

    def __init__(self, budget):
        self.budget = int(budget)
        self.lock = threading.RLock()
        self._d = collections.OrderedDict()
        self._size = {}
        self._total = 0

    def get(self, key):
        with self.lock:
            v = self._d.get(key)
            if v is not None:
                self._d.move_to_end(key)
            return v

    def put(self, key, value):
        with self.lock:
            old = self._d.pop(key, None)
            if old is not None:
                self._total -= self._size.pop(key, 0)
            self._d[key] = value
            self._size[key] = _nbytes(value)
            self._total += self._size[key]
            self._evict()
            return value

    def resize(self, key):
        """条目里追加了子字段（vig/noise/mask…）后重新记账。"""
        with self.lock:
            if key not in self._d:
                return
            self._total -= self._size.get(key, 0)
            self._size[key] = _nbytes(self._d[key])
            self._total += self._size[key]
            self._evict()

    def _evict(self):
        # 至少留一条：正在用的那张即使超预算也不能当场丢掉
        while self._total > self.budget and len(self._d) > 1:
            k = next(iter(self._d))
            self._d.pop(k, None)
            self._total -= self._size.pop(k, 0)

    def total(self):
        with self.lock:
            return self._total

    def __len__(self):
        with self.lock:
            return len(self._d)


_GEOM = _BudgetCache(768 << 20)


def _radius(H, W):
    """到画面中心的椭圆归一化半径 (H,W)，取值 0~1。"""
    key = (H, W)
    with _GEOM.lock:
        g = _GEOM.get(key)
        if g is None:
            sx = (np.arange(W, dtype=np.float32) - (W - 1) / 2) / (W / 2)
            sy = (np.arange(H, dtype=np.float32) - (H - 1) / 2) / (H / 2)
            r = np.sqrt(sx[None, :] ** 2 + sy[:, None] ** 2) / np.sqrt(2.0)
            g = _GEOM.put(key, {"r": r.astype(np.float32)})
        return g["r"]


def _noise(H, W, size=1.0, freq=1.0):
    """固定种子的噪声场 (H,W,1)。freq 越小网格越粗，size 再做一次模糊。

    seed 固定是硬要求：延时序列逐帧渲染，种子一变颗粒就在视频里"沸腾"。
    freq=1、size=1 时逐像素取随机数，与旧版颗粒场完全一致。
    """
    key = (H, W)
    sz = float(np.clip(size, 1.0, 12.0))
    fq = float(np.clip(freq, 0.2, 1.0))
    with _GEOM.lock:
        g = _GEOM.get(key)
        if g is None:
            _radius(H, W)
            g = _GEOM.get(key)
        slot = g.get("noise")
        if not isinstance(slot, dict):
            slot = g["noise"] = {}
        nk = (round(sz, 2), round(fq, 3))
        n = slot.get(nk)
        if n is None:
            rng = np.random.default_rng(20240517)
            if fq >= 0.999 and sz <= 1.0:
                n = rng.standard_normal((H, W, 1), dtype=np.float32)
            else:
                sh = max(1, int(round(H * fq)))
                sw = max(1, int(round(W * fq)))
                small = rng.standard_normal((sh, sw, 1), dtype=np.float32)
                n = cv2.resize(small, (W, H), interpolation=cv2.INTER_LINEAR)
                k = int(round(sz))
                if k > 1:
                    n = cv2.blur(n, (2 * k + 1, 2 * k + 1))
                if n.ndim == 2:            # cv2 会把单通道压缩掉一维
                    n = n[..., None]
            slot[nk] = n
            if len(slot) > 2:
                slot.pop(next(iter(slot)))
            _GEOM.resize(key)
        return n


# 色相带权重只取决于色相，所以先把 8 个带的贡献合成三张一维表，
# 再按色相查表。逐像素算 8 遍全图 (H,W) 的 abs/mod/smoothstep 要 500ms+，
# 查表只要 40ms。分箱取 2 的幂：索引可以直接 & 取模，省掉一次昂贵的浮点 mod；
# 65536 箱（1/596 度）把查表量化误差压到 1/255 以下，肉眼与旧版一致。
_HUE_BINS = 65536
_HUE_AX = np.arange(_HUE_BINS, dtype=np.float32) * (360.0 / _HUE_BINS)

# HSL / 曲线表只跟各自的参数有关。拖曝光等别的滑块时它们没变，
# 每帧重建会白花 10ms 左右，按字节预算缓存最近几份；
# 16bit 曲线表一份不到 0.4MB，24MB 足够放十几套参数。
_TABLES = _BudgetCache(24 << 20)


def _cached(key, build):
    with _TABLES.lock:
        v = _TABLES.get(key)
        if v is None:
            v = _TABLES.put(key, build())
        return v


def _hsl_tables(p):
    """返回 (色相偏移, 饱和系数, 明度系数) 三张色相表；未启用的轴为 None。"""
    shift = np.zeros(_HUE_BINS, dtype=np.float32)
    sfac = np.zeros(_HUE_BINS, dtype=np.float32)
    lfac = np.zeros(_HUE_BINS, dtype=np.float32)
    on = [False, False, False]
    for i in range(8):
        hs, ss, ls = p["hsl_h"][i], p["hsl_s"][i], p["hsl_l"][i]
        if not (hs or ss or ls):
            continue
        d = np.abs((_HUE_AX - HSL_CENTERS[i] + 180.0) % 360.0 - 180.0)
        w = _smooth(50.0, 18.0, d)
        if hs:
            shift += (hs / 100.0) * 40.0 * w
            on[0] = True
        if ss:
            sfac += (ss / 100.0) * w
            on[1] = True
        if ls:
            lfac += (ls / 100.0) * 0.6 * w
            on[2] = True
    return tuple(t if f else None for t, f in zip((shift, sfac, lfac), on))


def _chan_range(x):
    """逐像素 max-min 与 min。用三条切片比 x.max(-1) 快一个数量级。"""
    a, b, c = x[..., 0], x[..., 1], x[..., 2]
    mx = np.maximum(np.maximum(a, b), c)
    return mx - np.minimum(np.minimum(a, b), c)


def _mono_table(p):
    """色相 -> 亮度系数（中性 1.0）。8 个色相带用与 HSL 相同的高斯型权重叠加。

    这就是 Lightroom B&W Mixer / Capture One 单色混合：红/橙往上走是提亮肤色，
    蓝往下压是压深天空。
    """
    tab = np.ones(_HUE_BINS, dtype=np.float32)
    for i in range(8):
        a = p.get("mono_h", ())[i] if len(p.get("mono_h", ())) > i else 0.0
        if not a:
            continue
        d = np.abs((_HUE_AX - HSL_CENTERS[i] + 180.0) % 360.0 - 180.0)
        tab += (a / 100.0) * _smooth(50.0, 18.0, d)
    return tab


def _mono(x, p):
    """转单色（按色相带混合亮度），mono 值同时充当彩色↔黑白的混合量。"""
    k = float(p.get("mono", 0.0)) / 100.0
    if k <= 0.0:
        return x
    x = np.clip(x, 0.0, 1.0)
    shp = x.shape
    src = x if x.ndim == 3 else x.reshape(-1, 1, 3)
    lum = src @ LUMA
    mh = p.get("mono_h") or ()
    if any(mh):
        h = cv2.cvtColor(np.ascontiguousarray(src),
                         cv2.COLOR_RGB2HSV)[..., 0]
        key = ("mono", tuple(mh))
        tab = _cached(key, lambda: _mono_table(p))
        idx = (h * (_HUE_BINS / 360.0)).astype(np.int32) & (_HUE_BINS - 1)
        lum = np.clip(lum * np.take(tab, idx), 0.0, 1.0)
    g = np.repeat(lum[..., None], 3, axis=2)
    return (src + (g - src) * k).reshape(shp)


# ---- 镜头校正 ----
# 三项都是空间运算，跟着 Capture One 的顺序放在管线最前：先把几何与边缘色差
# 修掉，后面的曝光/清晰度才不会作用在畸变残留上。
def _distort_map(H, W, k):
    """径向畸变 remap 表。k>0 = 桶形校正（边缘向外扩），k<0 反之。"""
    size = (H, W)
    with _GEOM.lock:
        g = _GEOM.get(size)
        if g is None:
            _radius(H, W)
            g = _GEOM.get(size)
        slot = g.setdefault("dist", {})
        key = round(float(k), 4)
        m = slot.get(key)
        if m is None:
            r = g["r"]
            yy, xx = np.mgrid[0:H, 0:W].astype(np.float32)
            # 归一化半径是「到角落为 1」的椭圆半径，缩放量按到中心距离走
            s = (1.0 + key * r * r)[..., None]
            c = np.array([(W - 1) * 0.5, (H - 1) * 0.5], dtype=np.float32)
            p = np.stack([xx, yy], axis=2) - c
            mapxy = np.ascontiguousarray(p * s + c)
            m = slot[key] = (mapxy[..., 0], mapxy[..., 1])
            if len(slot) > 3:
                slot.pop(next(iter(slot)))
            _GEOM.resize(size)
        return m


def _defringe(x, amount):
    """去紫边 / 绿边：只压边缘附近、单侧通道明显高出的那部分。

    平坦区域的通道差是真实颜色（紫花、绿叶），所以用亮度梯度加权后再决定压多少，
    这一点和 Camera Raw 的 defringing 一样是「跟着边缘走」的。
    """
    a = float(amount) / 100.0
    if a <= 0.0:
        return x
    lum = np.clip(x, 0.0, 1.0) @ LUMA
    gx = cv2.Sobel(lum, cv2.CV_32F, 1, 0, ksize=3)
    gy = cv2.Sobel(lum, cv2.CV_32F, 0, 1, ksize=3)
    edge = _smooth(0.010, 0.075, np.sqrt(gx * gx + gy * gy))
    r, g, b = x[..., 0], x[..., 1], x[..., 2]
    mx = np.maximum(r, b)
    pur = np.clip(np.minimum(r, b) - g, 0.0, None)     # 紫/品红：r、b 一起高于 g
    grn = np.clip(g - mx, 0.0, None)                   # 绿：g 高于 r、b
    wp = a * edge * np.clip(pur / 0.10, 0.0, 1.0)
    wg = a * edge * np.clip(grn / 0.10, 0.0, 1.0)
    out = x.copy()
    out[..., 0] = r - (r - g) * wp
    out[..., 2] = b - (b - g) * wp
    out[..., 1] = g - (g - mx) * wg
    return out


_PIVOT_GAMMA = np.float32(0.4496)      # ln(0.5) / ln(srgb_to_lin(0.5))
_PIVOT_INV = np.float32(2.2242)        # 1 / _PIVOT_GAMMA


def _contrast_s_lin(xl):
    """线性光域对比度的 S 曲线：绕中灰转，不是绕线性 0.5。

    直接对 xl 做 smoothstep 会在线性 0.5（编码 0.735）拐弯，等于把支点抬到
    73% 灰——实测 contrast=50 时中灰被压暗 14 级。这里先把 xl 重标定成
    u = xl^γ 使中灰（线性 ≈0.214）落到 0.5，做完 smoothstep 再拉回来，
    于是 S(0)=0、S(中灰)=中灰、S(1)=1。与 Rust develop.rs::contrast_s_lin 同式。
    """
    u = np.power(np.maximum(xl, 0.0), _PIVOT_GAMMA)
    s = u * u * (np.float32(3.0) - np.float32(2.0) * u)
    return np.power(s, _PIVOT_INV)


def _lens(x, p):
    d = p.get("lens_distort", 0.0)
    if d:
        H, W = x.shape[:2]
        mx, my = _distort_map(H, W, float(d) / 100.0 * 0.22)
        x = cv2.remap(x, mx, my, cv2.INTER_LINEAR,
                      borderMode=cv2.BORDER_REPLICATE)
    lv = p.get("lens_vignette", 0.0)
    if lv:
        H, W = x.shape[:2]
        m = 1.0 + (float(lv) / 100.0) * 0.9 * _vignette_falloff(H, W)
        x = x * np.clip(m, 0.0, 2.0)[..., None]
    fr = p.get("lens_fringe", 0.0)
    if fr:
        x = _defringe(np.clip(x, 0.0, 1.0), fr)
    # 只压负值，向上不封顶：影调链要靠 >1 的亮度做曲线尾部衔接，HDR 那条路也
    # 指望这一步别把动态范围削平。负值必须压掉，_lin_to_srgb 对负数做 pow 会出 NaN。
    # 旧口径是 np.clip(x, 0, 1)，与 Rust spatial::lens 的 clamp01 一一对应。
    return np.maximum(x, 0.0)


# ---- 局部蒙版 ----
# Linear / Radial 渐变两张场，几何存成归一化坐标所以只跟尺寸有关；
# 每张蒙版的参数是「相对全局的增量」，渲染时把叠好增量的整条管线结果
# 按权重混回来 —— 这样局部曝光/色温和全局是同一套算法，不会有第二套语义。
def _any_delta(d):
    return any(abs(float(v)) > 1e-6 for v in d.values())


def mask_line_px(m, W, H):
    """蒙版几何的显示端点（像素坐标），已按 angle 旋转。

    linear 绕两端中点旋转整条渐变线；radial 圆心不动，只旋转边缘点。
    画面把手与实际权重场共用这一份几何，拖起来才不会错位。
    """
    ax, ay = float(m["x1"]) * (W - 1), float(m["y1"]) * (H - 1)
    bx, by = float(m["x2"]) * (W - 1), float(m["y2"]) * (H - 1)
    ang = float(m.get("angle") or 0.0)
    if ang:
        rad = np.deg2rad(ang)
        c, s = np.cos(rad), np.sin(rad)
        if _kind(m) == "radial":
            dx, dy = bx - ax, by - ay
            bx, by = ax + dx * c - dy * s, ay + dx * s + dy * c
        else:
            mx, my = (ax + bx) * 0.5, (ay + by) * 0.5
            dx, dy = ax - mx, ay - my
            ax, ay = mx + dx * c - dy * s, my + dx * s + dy * c
            dx, dy = bx - mx, by - my
            bx, by = mx + dx * c - dy * s, my + dx * s + dy * c
    return ax, ay, bx, by


def _range_weight(H, W, lum, rm):
    """亮度范围的柔和权重 (H,W)；lum 可与输出尺寸不同，按输出尺寸返回。"""
    if not rm or not rm.get("lum_on") or lum is None:
        return None
    src = lum
    if src.ndim > 2:
        src = src @ LUMA
    if src.shape[:2] != (H, W):
        src = cv2.resize(np.ascontiguousarray(src), (W, H),
                         interpolation=cv2.INTER_AREA)
    lo = float(np.clip(rm.get("lum_min", 0.0), 0.0, 1.0))
    hi = float(np.clip(rm.get("lum_max", 1.0), 0.0, 1.0))
    if hi < lo:
        lo, hi = hi, lo
    fe = max(float(rm.get("lum_feather", 0.1)), 1e-3)
    wl = _smooth(lo - fe, lo, src)
    wh = 1.0 - _smooth(hi, hi + fe, src)
    return np.minimum(wl, wh).astype(np.float32)


def _apply_range(w, H, W, lum, rm):
    rw = _range_weight(H, W, lum, rm)
    return w if rw is None else w * rw


def _comp_field(H, W, comp):
    """单个形状组件的几何权重场 (H,W)，0~1；不含组强度。"""
    size = (H, W)
    key = (_kind(comp), round(float(comp["x1"]), 4),
           round(float(comp["y1"]), 4), round(float(comp["x2"]), 4),
           round(float(comp["y2"]), 4),
           round(float(comp["feather"]), 1), 1 if comp.get("invert") else 0,
           round(float(comp.get("angle") or 0.0), 2),
           round(float(comp.get("midpoint", 50.0) or 0.0), 2))
    with _GEOM.lock:
        g = _GEOM.get(size)
        if g is None:
            _radius(H, W)
            g = _GEOM.get(size)
        slot = g.setdefault("mask", {})
        w = slot.get(key)
        if w is None:
            fe = float(np.clip(comp["feather"], 0.0, 100.0)) / 100.0
            mp = float(np.clip(comp.get("midpoint", 50.0),
                               0.0, 100.0)) / 100.0
            yy, xx = np.mgrid[0:H, 0:W].astype(np.float32)
            if _kind(comp) == "radial":
                # 像素坐标里算椭圆：归一化坐标会把 16:9 的圆拉成横扁的圈
                cx, cy = float(comp["x1"]) * (W - 1), float(comp["y1"]) * (H - 1)
                dx = (float(comp["x2"]) - float(comp["x1"])) * (W - 1)
                dy = (float(comp["y2"]) - float(comp["y1"])) * (H - 1)
                rx, ry = abs(dx), abs(dy)
                if ry < max(H * 0.01, 1.0):        # 只横向拖 = 正圆
                    ry = max(rx, 1.0)
                elif rx < max(W * 0.01, 1.0):      # 只纵向拖 = 正圆
                    rx = max(ry, 1.0)
                ang = float(comp.get("angle") or 0.0)
                if ang:
                    # 椭圆绕中心转 ang：把像素坐标反旋回椭圆自身坐标系
                    rad = np.deg2rad(ang)
                    co, si = np.cos(rad), np.sin(rad)
                    u = (xx - cx) * co + (yy - cy) * si
                    v = -(xx - cx) * si + (yy - cy) * co
                else:
                    u, v = xx - cx, yy - cy
                q = np.hypot(u / rx, v / ry)
                q = q - (mp - 0.5) * (0.5 + fe)
                # 圈内为 1；feather 0 时过渡带宽度为 0，_smooth 会除以零，直接硬边
                w = (q <= 1.0).astype(np.float32) if fe <= 0.0 \
                    else _smooth(1.0 + fe, 1.0 - fe, q)
            else:
                # 同样按像素距离投影：过渡线与拖拽方向在画面上真的垂直
                ax, ay, bx, by = mask_line_px(comp, W, H)
                bx, by = bx - ax, by - ay
                L2 = max(bx * bx + by * by, 1e-6)
                t = ((xx - ax) * bx + (yy - ay) * by) / L2
                t = t - (mp - 0.5)
                w = _smooth(1.0 + fe, -fe, t)           # 起点一侧为 1
            if comp.get("invert"):
                w = 1.0 - w
            w = slot[key] = np.ascontiguousarray(w, dtype=np.float32)
            if len(slot) > 3:
                slot.pop(next(iter(slot)))
            _GEOM.resize(size)
        return w


def group_field(H, W, g, lum=None):
    """把一个组的所有组件合成为单张权重场 0~1。

    Intersect = 相乘；Add = min(1, Σ)；Subtract = max(0, a-b)。
    导出/视频前必须先合成 —— 一个组只写一张权重图、一条 overlay 支路。
    """
    acc = None
    for comp in g.get("components") or ():
        w = _comp_field(H, W, comp)
        w = _apply_range(w, H, W, lum, comp.get("range"))
        blend = comp.get("blend", "add")
        if acc is None or blend == "add":
            acc = w if acc is None else np.minimum(1.0, acc + w)
        elif blend == "subtract":
            acc = np.maximum(0.0, acc - w)
        else:                                    # intersect
            acc = acc * w
    if acc is None:
        return None
    amt = float(np.clip(g.get("amount", 100.0), 0.0, 100.0)) / 100.0
    if amt < 1.0:
        acc = acc * amt
    if g.get("invert"):
        acc = 1.0 - acc
    acc = _apply_range(acc, H, W, lum, g.get("range"))
    return np.ascontiguousarray(np.clip(acc, 0.0, 1.0), dtype=np.float32)


def _mask_field(H, W, m, lum=None):
    """蒙版权重场 (H,W)，0~1；扁平老蒙版与组都走这里。"""
    if "components" in m:
        return group_field(H, W, m, lum=lum)
    w = _comp_field(H, W, m)
    w = _apply_range(w, H, W, lum, m.get("range"))
    amt = float(np.clip(m.get("amount", 100.0), 0.0, 100.0)) / 100.0
    if amt < 1.0:
        w = w * amt
    return np.ascontiguousarray(w, dtype=np.float32)


def masks_use_range(p):
    """参数里是否有蒙版启用亮度范围（导出侧要先算一张亮度图）。"""
    for m in p.get("masks") or ():
        if (m.get("range") or {}).get("lum_on"):
            return True
        for comp in m.get("components") or ():
            if (comp.get("range") or {}).get("lum_on"):
                return True
    return False


def _blend_masks(x, x0, p, ms, fast, lum=None):
    H, W = x.shape[:2]
    out = x
    for m in ms:
        d = m.get("params") or {}
        # 只有空间项（锐化/降噪）的组不重跑整条管线，留给后面的单遍空间处理
        if not _any_delta({k: v for k, v in d.items() if k in MASK_KEYS}):
            continue
        q = _mask_delta(p, d)
        xb = _core(x0, q, for_lut=False, fast=fast, local=True)
        out = out + (xb - out) * _mask_field(H, W, m, lum=lum)[..., None]
    return np.clip(out, 0.0, 1.0)


def _mask_spatial(x, p, lum=None):
    """按组权重在合成图上施加一次锐化/降噪。

    放进 _core 的局部重跑会让 halo 被该组自己的曲线 tone-map 两次，而且
    N 张蒙版要跑 N 次全图卷积。这里取各组上限强度跑一次，再按权重混回。
    """
    H, W = x.shape[:2]
    w_sharp = w_nr = None
    s_max = l_max = c_max = 0.0
    s_tune = (1.0, 25.0, 0.0)
    n_tune = (0.0, 0.0, 50.0)
    n_best = -1.0
    for g in _mask_active(p):
        d = g.get("params") or {}
        sa = max(0.0, float(d.get("sharp_amount", 0.0) or 0.0))
        nl = max(0.0, float(d.get("nr_luma", 0.0) or 0.0))
        nc = max(0.0, float(d.get("nr_color", 0.0) or 0.0))
        if sa <= 1e-6 and nl <= 1e-6 and nc <= 1e-6:
            continue
        field = _mask_field(H, W, g, lum=lum)
        if sa > 1e-6:
            w = np.clip(field * (sa / 100.0), 0.0, 1.0)
            w_sharp = w if w_sharp is None else np.maximum(w_sharp, w)
            if sa > s_max:
                s_max = sa
                s_tune = (float(d.get("sharp_radius", 1.0) or 1.0),
                          float(d.get("sharp_detail", 25.0) or 0.0),
                          float(d.get("sharp_masking", 0.0) or 0.0))
        if nl > 1e-6 or nc > 1e-6:
            w = np.clip(field * (max(nl, nc) / 100.0), 0.0, 1.0)
            w_nr = w if w_nr is None else np.maximum(w_nr, w)
            l_max, c_max = max(l_max, nl), max(c_max, nc)
            if max(nl, nc) > n_best:
                n_best = max(nl, nc)
                csm = d.get("nr_color_smooth")
                n_tune = (float(d.get("nr_luma_detail", 0.0) or 0.0),
                          float(d.get("nr_luma_contrast", 0.0) or 0.0),
                          float(50.0 if csm is None else csm))
    if w_sharp is None and w_nr is None:
        return x
    if w_nr is not None:
        xs = _denoise(x, l_max, c_max, *n_tune)
        x = x + (xs - x) * w_nr[..., None]
    if w_sharp is not None:
        xs = _sharpen(x, s_max, *s_tune)
        x = x + (xs - x) * w_sharp[..., None]
    return np.clip(x, 0.0, 1.0)


# ---- 给 GUI / 导出用的公开入口 ----
def active_masks(p):
    """会被渲染的蒙版。视频导出要为每张追加一条「LUT + 权重图」支路。"""
    return _mask_active(p)


def mask_params(p, m):
    """全局参数叠上这张蒙版的增量 —— 只在蒙版区域内生效的那套参数。"""
    return _mask_delta(p, m.get("params") or {})


def mask_field(H, W, m, lum=None):
    """蒙版权重场 (H,W) float32 0~1，导出成 PNG 给 ffmpeg 当 alpha。"""
    return _mask_field(H, W, m, lum=lum)


def _levels_active(p):
    return bool(p.get("lv_black", 0.0) or p.get("lv_gamma", 1.0) != 1.0
                or p.get("lv_white", 255.0) != 255.0
                or p.get("lv_out_black", 0.0)
                or p.get("lv_out_white", 255.0) != 255.0)


def _levels_lut(p, n=256):
    """输入黑白场 + 中间调 gamma + 输出黑白场 -> n 级 [0,1] 表。

    和 Photoshop 的色阶同一套语义：先把输入裁到 [black, white] 再套 gamma
    （gamma>1 提亮中间调），最后压进 [out_black, out_white]。
    """
    v = np.linspace(0.0, 1.0, int(n), dtype=np.float32)
    lo = float(np.clip(p.get("lv_black", 0.0), 0.0, 253.0)) / 255.0
    hi = float(np.clip(p.get("lv_white", 255.0), lo + 2.0 / 255.0, 255.0)) / 255.0
    t = np.clip((v - lo) / (hi - lo), 0.0, 1.0)
    g = float(np.clip(p.get("lv_gamma", 1.0), 0.1, 5.0))
    if g != 1.0:
        t = t ** (1.0 / g)
    ob = float(np.clip(p.get("lv_out_black", 0.0), 0.0, 255.0)) / 255.0
    ow = float(np.clip(p.get("lv_out_white", 255.0), ob + 2.0 / 255.0, 255.0)) / 255.0
    return ob + t * (ow - ob)


_CURVE_N = 65536      # 16bit 表：8bit 合表会在暗部渐变上留下可见的台阶
_PAR_SCALE = 0.18     # 参数曲线 ±100 在对应亮度区最多推 18% 亮度


def _par_active(p):
    return any(abs(float(p.get(k, 0.0) or 0.0)) > 1e-6
               for k in ("par_shadows", "par_darks",
                         "par_lights", "par_highlights"))


def _par_lut(p, n=_CURVE_N):
    """参数曲线：阴影/暗部/亮部/高光四段权重 + 三个分割点 -> 亮度映射表。

    四段用 smoothstep 做钟形权重并覆盖整条亮度轴，往中间调推只会抬该段；
    偏移加在输入亮度上，再 cummax 保证严格不减（滑块打到极端也不会反转）。
    """
    v = np.linspace(0.0, 1.0, int(n), dtype=np.float32)
    s1 = float(np.clip(p.get("par_split1", 25.0) or 0.0, 2.0, 86.0)) / 100.0
    s2 = float(np.clip(p.get("par_split2", 50.0) or 0.0,
                       s1 * 100.0 + 2.0, 92.0)) / 100.0
    s3 = float(np.clip(p.get("par_split3", 75.0) or 0.0,
                       s2 * 100.0 + 2.0, 98.0)) / 100.0
    w_sh = 1.0 - _smooth(0.0, s1, v)
    w_dark = _smooth(0.0, s1, v) * (1.0 - _smooth(s1, s2, v))
    w_light = _smooth(s1, s2, v) * (1.0 - _smooth(s2, s3, v))
    w_hi = _smooth(s2, s3, v)
    a = float(np.clip(p.get("par_shadows", 0.0) or 0.0, -100.0, 100.0))
    b = float(np.clip(p.get("par_darks", 0.0) or 0.0, -100.0, 100.0))
    c = float(np.clip(p.get("par_lights", 0.0) or 0.0, -100.0, 100.0))
    d = float(np.clip(p.get("par_highlights", 0.0) or 0.0, -100.0, 100.0))
    off = (a * w_sh + b * w_dark + c * w_light + d * w_hi) / 100.0
    out = np.clip(v + off * _PAR_SCALE, 0.0, 1.0)
    return np.maximum.accumulate(out).astype(np.float32)


def _curve_lut(p):
    """色阶 + 参数曲线 + 分通道曲线 + 主曲线合成一张 (65536,1,3) 的 uint16 表。

    四级预先合表，交给 cv2.LUT 一次扫完。用 16bit 而不是 8bit：曲线在
    低光区斜率大，8bit 输入量化会把 0.5 个台阶放大成两三个台阶，暗部
    渐变出现色带；65536 级表把这一步的量化误差压到看不见。
    """
    n = _CURVE_N
    lv = _levels_lut(p, n) if _levels_active(p) else None
    lv_idx = None if lv is None else np.clip(
        np.rint(lv * (n - 1)), 0, n - 1).astype(np.int32)
    par = _par_lut(p, n) if _par_active(p) else None
    par_idx = None if par is None else np.clip(
        np.rint(par * (n - 1)), 0, n - 1).astype(np.int32)
    master = pchip_lut(p["curve_rgb"], n) / 255.0
    tabs = []
    for key in ("curve_r", "curve_g", "curve_b"):
        lt = pchip_lut(p[key], n) / 255.0
        if par_idx is None:
            base_idx = lv_idx
        elif lv_idx is None:
            base_idx = par_idx
        else:
            # 色阶输出先过参数曲线，再进分通道曲线
            base_idx = np.clip(np.rint(par[lv_idx] * (n - 1)),
                               0, n - 1).astype(np.int32)
        y = lt if base_idx is None else lt[base_idx]
        inner = np.clip(np.rint(y * (n - 1)), 0, n - 1).astype(np.int32)
        tabs.append(np.rint(master[inner] * 65535.0).astype(np.uint16))
    return np.stack(tabs, axis=1).reshape(n, 1, 3)


# ---- 线性光：白平衡与曝光 ----
# Camera Raw / Capture One 都在线性光里做白平衡和曝光。这里把「sRGB→线性→
# 增益/曝光→sRGB」合成为一张 4096 级的逐通道表，12bit 量化后一次查表完成，
# 比两次 np.power 快一倍多；亮度归一化保证调色温时整体明暗不跟着漂。
_LIN_N = 4096


def _srgb_to_lin(v):
    return np.where(v <= 0.04045, v / 12.92,
                    ((v + 0.055) / 1.055) ** 2.4)


def _lin_to_srgb(v):
    v = np.maximum(v, 0.0)
    return np.where(v <= 0.0031308, v * 12.92,
                    1.055 * v ** (1.0 / 2.4) - 0.055)


def _wb_table(t, u, ev, chan=None):
    v = np.linspace(0.0, 1.0, _LIN_N, dtype=np.float32)
    lin = _srgb_to_lin(v)[:, None]
    gains = np.array([1.0 + 0.30 * t + 0.08 * u,
                      1.0 - 0.22 * u,
                      1.0 - 0.30 * t + 0.08 * u], dtype=np.float32)
    gains /= float(gains @ LUMA)
    shift = np.float32(ev)
    if chan is not None:
        # 分通道去闪：每通道各自的 EV 修正，叠在整帧曝光上
        shift = shift + np.asarray(chan, dtype=np.float32)
    else:
        shift = np.full(3, shift, dtype=np.float32)
    out = _lin_to_srgb(lin * (gains * np.power(np.float32(2.0), shift))[None, :])
    return np.clip(out, 0.0, 4.0).astype(np.float32)


def chan_ev(p):
    """参数里的逐通道曝光修正（R,G,B 顺序）；没有或全 0 返回 None。"""
    c = p.get("chan_ev")
    if not isinstance(c, (list, tuple)) or len(c) != 3:
        return None
    try:
        v = [float(x) for x in c]
    except (TypeError, ValueError):
        return None
    if not all(math.isfinite(x) for x in v):
        return None
    return None if max(abs(x) for x in v) < 1e-4 else v


def _wb_exposure(x, t, u, ev, chan=None):
    if t == 0.0 and u == 0.0 and ev == 0.0 and not chan:
        return x
    key = ("wbexp", round(t, 4), round(u, 4), round(ev, 3),
           tuple(round(c, 3) for c in chan) if chan else None)
    tab = _cached(key, lambda: _wb_table(t, u, ev, chan))
    idx = np.clip(x * (_LIN_N - 1) + 0.5, 0, _LIN_N - 1).astype(np.int32)
    idx *= 3                      # 摊平成 (级别, 通道) 的线性下标，
    idx += np.arange(3, dtype=np.int32)   # 避免 tab[idx] 多出一维通道
    return tab.ravel()[idx]


# ---- 边缘感知（引导滤波）----
# 自实现的 O(1) 盒式引导滤波：清晰度、纹理、亮度降噪共用同一套核。
# OpenCV 的 contrib 版 guidedFilter 在 headless 包里没有，因此用 cv2.blur 拼。
def _guided_coeff(I, p, r, eps):
    mI = cv2.blur(I, (2 * r + 1, 2 * r + 1))
    mp = cv2.blur(p, (2 * r + 1, 2 * r + 1))
    var = cv2.blur(I * I, (2 * r + 1, 2 * r + 1)) - mI * mI
    cov = cv2.blur(I * p, (2 * r + 1, 2 * r + 1)) - mI * mp
    a = cov / (var + eps)
    return a, mp - a * mI


def _guided(I, p, r, eps):
    """大半径时在低分辨率上算系数再上采样（fast guided filter）。

    清晰度半径常到几十像素，全分辨率四次盒滤波在 3MP 上要 200ms+；
    系数本身是平滑的，1/4 尺寸算完上采样，肉眼看不出差别。
    """
    H, W = I.shape[:2]
    s = 4 if r >= 8 else (2 if r >= 4 else 1)
    if s == 1 or min(H, W) < 64:
        a, b = _guided_coeff(I, p, r, eps)
        return cv2.blur(a, (2 * r + 1, 2 * r + 1)) * I \
            + cv2.blur(b, (2 * r + 1, 2 * r + 1))
    Is = cv2.resize(I, (max(1, W // s), max(1, H // s)),
                    interpolation=cv2.INTER_AREA)
    ps = cv2.resize(p, (Is.shape[1], Is.shape[0]),
                    interpolation=cv2.INTER_AREA)
    a, b = _guided_coeff(Is, ps, max(1, int(round(r / s))), eps)
    a = cv2.resize(a, (W, H), interpolation=cv2.INTER_LINEAR)
    b = cv2.resize(b, (W, H), interpolation=cv2.INTER_LINEAR)
    return a * I + b


def _local_contrast(x, amount, radius, eps, scale):
    """清晰度 / 纹理：拉大「像素 - 局部均值」的差。

    普通高斯 USM 在强边缘两侧造白边黑边；引导滤波在边缘处 a→1、base≈原图，
    差值自然归零，所以光晕被结构性地压住了（Capture One Clarity 的思路）。
    """
    lum = x @ LUMA
    detail = lum - _guided(lum, lum, radius, eps)
    mid = 1.0 - (2.0 * np.clip(lum, 0.0, 1.0) - 1.0) ** 2
    detail *= 0.35 + 0.65 * mid
    ratio = np.clip((lum + amount * scale * detail)
                    / np.maximum(lum, 1e-4), 0.0, 3.0)
    return x * ratio[..., None]


def _denoise(x, luma_amt, color_amt, luma_detail=0.0, luma_contrast=0.0,
             color_smooth=50.0, multiscale=0.0, sigma_manual=0.0):
    """色度先降噪（亮度当引导，颜色不糊到边缘外），再做亮度降噪。

    detail 越高，亮度引导的 eps 越小（更贴合原图）：只磨真正的噪点、
    保住星星与纹理边缘，同时靠 res 的补偿把降噪量拉回同一水平。
    luma_contrast 把磨掉的局部对比还一部分；color_smooth 越高色块越平。
    三个副参数取默认值时，结果与旧版逐像素一致。

    多尺度 + σ 噪声模型（设计 2026-10-30 §1.3，与 Rust spatial.rs::denoise_apply
    逐式对应）：multiscale>0 或 sigma_manual>0 且 luma_amt>0 时走多尺度分支
    （粗层引导 r_c=min/130 + σ 驱动 eps）；全 0 时走旧单尺度路径逐位一致。
    色度通道不动（色噪以低频为主）。
    """
    H, W = x.shape[:2]
    d = float(np.clip(luma_detail, 0.0, 100.0)) / 100.0
    cs = float(np.clip(color_smooth, 0.0, 100.0)) / 100.0
    if color_amt:
        ycc = cv2.cvtColor(np.ascontiguousarray(x), cv2.COLOR_RGB2YCrCb)
        y = ycc[..., 0]
        k = float(color_amt) / 100.0
        r = max(2, int(min(H, W) / 180))
        eps = 0.0002 + 0.008 * k * (0.3 + 1.4 * cs)
        for c in (1, 2):
            p = ycc[..., c]
            sm = _guided(y, p, r, eps)
            ycc[..., c] = p + (sm - p) * k
        x = cv2.cvtColor(ycc, cv2.COLOR_YCrCb2RGB)
    if luma_amt:
        lum = x @ LUMA
        k = float(luma_amt) / 100.0
        ms = float(np.clip(multiscale, 0.0, 100.0)) / 100.0
        sv = float(np.clip(sigma_manual, 0.0, 100.0))
        if ms == 0.0 and sv == 0.0:
            # 旧单尺度路径：与升级前逐位一致
            r = max(2, int(min(H, W) / 260))
            eps = 0.0001 + 0.006 * k
            sm = _guided(lum, lum, r, eps)
            # 加性回写而不是按比例缩放：暗部不会把残差放大成新的色噪
            res = (sm - lum) * k
            if d:
                # Detail 不是把 eps 调小（那会让平坦区噪声回流），而是把被磨掉的
                # 结构按「显著高于噪声」的门限还回去：星星、纹理边缘恢复，平坦
                # 天空一点噪都不加。噪声 σ 用残差的中位数绝对偏差现场估计。
                hf = lum - cv2.blur(lum, (5, 5))
                samp = hf[::4, ::4]
                sig = 1.4826 * float(np.median(np.abs(
                    samp - np.median(samp))))
                m = np.clip((np.abs(hf) / max(sig, 1e-4) - 3.0) / 3.0, 0.0, 1.0)
                m = cv2.dilate(m, np.ones((3, 3), np.uint8)) * m ** 0.3
                res = res + (lum - sm) * (k * 3.0 * d) * m
            if luma_contrast:                                  # 把磨掉的局部对比还回来
                hf = lum - cv2.blur(lum, (5, 5))
                res = res + hf * (float(luma_contrast) / 100.0) * 0.25
        else:
            # σ 估计每帧一次（::4 采样，O(n/16)，不缓存）；手动覆盖优先
            # （与 LR 一致）。manual_map: σ = 0.002 + (v/100)^1.6·0.030，TODO(fit)。
            blur5 = cv2.blur(lum, (5, 5))
            hf = lum - blur5
            samp = hf[::4, ::4]
            sig_a = 1.4826 * float(np.median(np.abs(samp - np.median(samp))))
            if sv > 0.0:
                sig = 0.002 + (sv / 100.0) ** 1.6 * 0.030
            else:
                sig = sig_a
            # eps = C·σ̂²/rm（方差量纲）：eps 就是噪声方差，1/rm 补偿「半径越大、
            # 同一个 a=cov/(var+eps) 铺开的邻域越大」带来的额外平滑。与 Rust
            # spatial.rs::denoise_apply 同式同构——整数半径的取法与顺序一样，
            # 乘除也按 f32 逐步舍入（(C·σ̂)·σ̂ / rm），两边 eps 逐位相同。
            rc = max(2, int(min(H, W) / 130))
            rm = max(2, int(min(H, W) / 260))
            s32 = np.float32(sig)
            eps = float(max((NR_MS_EPS_C * s32 * s32) / np.float32(rm),
                            NR_MS_EPS_MIN))
            b_coarse = _guided(lum, lum, rc, eps)
            b_mid = _guided(b_coarse, b_coarse, rm, eps)
            # 粗层只回写低频差异，量被 s 门控并入中尺度结果
            sm = b_mid + np.float32(ms) * (b_coarse - b_mid)
            res = (sm - lum) * k
            if d:
                # 门限还回逻辑不变，作用在 mid 层残差（lum − B_mid）上
                m = np.clip((np.abs(hf) / max(sig_a, 1e-4) - 3.0) / 3.0, 0.0, 1.0)
                m = cv2.dilate(m, np.ones((3, 3), np.uint8)) * m ** 0.3
                res = res + (lum - b_mid) * (k * 3.0 * d) * m
            if luma_contrast:                                  # 把磨掉的局部对比还回来
                res = res + hf * (float(luma_contrast) / 100.0) * 0.25
        x = x + res[..., None]
    return np.clip(x, 0.0, 1.0)


def _min_filter(m, r):
    """半径 r 的最小滤波：r 轮 3×3 最小迭代。

    **有效半径 = 轮数**：每轮 3×3 最小只把极值向外扩 1px，所以 r 轮才是严格
    意义的 r 半径最小滤波。历史上取 ⌈r/2⌉ 轮（有效半径只有名义值一半），
    `dehaze.rs` 的扫参表把两者都跑过：r 轮 PSNR 16.14 / 透射 RMSE 0.0930，
    ⌈r/2⌉ 轮 16.04 / 0.0955，两个尺子上都更好，因此改成 r 轮。

    边界用 BORDER_REFLECT_101（np.pad mode='reflect' 即 reflect_101），
    与 Rust dehaze.rs::min_filter 逐式对应。
    """
    if m.shape[0] < 2 or m.shape[1] < 2:
        return m.copy()
    cur = m
    for _ in range(r):
        p = np.pad(cur, 1, mode="reflect")
        cur = np.minimum(np.minimum(
            np.minimum(np.minimum(p[:-2, :-2], p[:-2, 1:-1]), p[:-2, 2:]),
            np.minimum(np.minimum(p[1:-1, :-2], p[1:-1, 1:-1]), p[1:-1, 2:])),
            np.minimum(np.minimum(p[2:, :-2], p[2:, 1:-1]), p[2:, 2:]))
    return cur.astype(np.float32)


def _dehaze_dcp(x, amount, contrast=0.0):
    """暗通道先验去雾（He DCP 2010/2011 公开数学 + E1 结构事实）。

    设计 2026-10-30 §2.3，与 Rust develop/dehaze.rs::dehaze_dcp 逐式对应：
    大气光单点估计（暗通道最亮 0.1% 里取线性亮度最高者，下限 1e-3）→
    归一化暗通道 + 最小滤波 → 透射初值 t = clamp(1−0.95·dark, 0.1, 1) →
    引导滤波精修 → J = (L−A)/max(t,0.1)+A → 按 amount 增量混合 →
    对比微调（输出亮度 [p1,p99] 黑位/白位重映射，幅度 contrast·0.15，
    contrast=0 时严格恒等）。去雾在线性域做（E3 管线事实）。
    t0=0.1 / eps_t=1e-4 / w=0.95 全部 TODO(fit)，不做滑杆暴露。
    """
    if amount <= 0.0:
        return x
    H, W = x.shape[:2]
    L = _srgb_to_lin(np.clip(x, 0.0, 1.0)).astype(np.float32)
    lum = (L @ LUMA).astype(np.float32)
    dark0 = L.min(axis=-1)

    # 1) 大气光 A：暗通道最亮的 0.1% 像素里取亮度最高者（并列取首个，
    #    与 numpy argmax 一致）；分量下限 1e-3。
    k = max(1, int(math.ceil(H * W * 0.001)))
    thr = np.sort(dark0.ravel())[H * W - k]
    idx = np.flatnonzero(dark0 >= thr)
    best = idx[int(np.argmax(lum.ravel()[idx]))]
    A = np.maximum(L.reshape(-1, 3)[best], np.float32(1e-3))

    # 2) 归一化暗通道（E1：先除 A 再 min）+ 最小滤波；3) 透射初值。
    r_d = max(2, int(min(H, W) / 200))
    m = (L / A).min(axis=-1)
    dark = _min_filter(m.astype(np.float32), r_d)
    t = np.clip(np.float32(1.0) - np.float32(0.95) * dark,
                np.float32(0.1), np.float32(1.0))

    # 4) 引导滤波精修（E1 线性模型 a·I+b，引导图 = 线性亮度）。
    r_t = 4 * r_d
    t = _guided(lum, t.astype(np.float32), r_t, 1e-4)
    t = np.clip(t, np.float32(0.1), np.float32(1.0))

    # 5) 合成 + 按 amount 增量混合。
    J = (L - A) / np.maximum(t, np.float32(0.1))[..., None] + A
    l_out = L + np.float32(amount) * (J - L)

    # 6) 对比微调：输出亮度 [p1,p99] 的黑位/白位重映射，幅度 contrast·0.15。
    ck = float(contrast)
    if ck != 0.0:
        mm = np.float32(ck * 0.15)
        lum_out = (l_out @ LUMA).astype(np.float32)
        p1 = np.float32(np.percentile(lum_out, 1.0))
        p99 = np.float32(np.percentile(lum_out, 99.0))
        lo = mm * p1
        hi = np.float32(1.0) - mm * (np.float32(1.0) - p99)
        l_out = np.clip((l_out - lo) / max(float(hi - lo), 1e-4), 0.0, 1.0)

    return np.clip(_lin_to_srgb(l_out), 0.0, 1.0)


def _sharpen(x, amount, radius, detail, masking, protect=0.0):
    """USM 锐化：detail 混入更小半径的高频，masking 只保留明显边缘。

    tanh 软限幅就是 capture sharpening 里 halo control 的简化：把过冲压在
    固定范围内，强边缘不会出现刺眼的白边/黑边。
    protect 是恒开边缘保护（对齐 LR/ACR Sharpen3 的保护掩码基础设施；
    阈值需拟合）：噪声底用 MAD σ 现场估计，平坦噪区不锐化。与 Rust
    spatial.rs::sharpen_apply 逐式对应。
    """
    lum = x @ LUMA
    r = float(np.clip(radius, 0.5, 3.0))
    base = lum - cv2.GaussianBlur(lum, (0, 0), r)
    d = float(np.clip(detail, 0.0, 100.0)) / 100.0
    if d:
        fine = lum - cv2.GaussianBlur(lum, (0, 0), max(0.6, r * 0.5))
        base = base * (1.0 - d) + fine * (1.1 * d)
    halo = 0.08
    base = halo * np.tanh(base / halo)
    m = float(np.clip(masking, 0.0, 100.0)) / 100.0
    pw = float(np.clip(protect, 0.0, 100.0)) / 100.0 * 0.9
    if m or pw > 0.0:
        edge = cv2.GaussianBlur(np.abs(base), (0, 0), max(1.0, r * 2.0))
        if pw > 0.0:
            samp = base[::4, ::4]
            sig = 1.4826 * float(np.median(np.abs(
                samp - np.median(samp))))
            t0 = sig * 1.2 + 0.0008
            t1 = t0 + sig * 2.0 + 0.004
            base = base * (1.0 - pw * (1.0 - _smooth(t0, t1, edge)))
        if m:
            # 平方根而不是平方：旧的 m² 把前段压平，0–50 对平坦噪声完全不生效。
            # 端点不动（m=0 仍是 0.001、m=1 仍是 0.021），中段才真正起作用。
            thr = 0.001 + 0.02 * np.sqrt(m)
            base = base * _smooth(thr * 0.35, thr, edge)
    gain = max(0.0, float(amount)) / 100.0 * 1.2
    ratio = np.clip((lum + gain * base) / np.maximum(lum, 1e-4), 0.0, 3.0)
    return x * ratio[..., None]


# ---- 三段式色彩分级 ----
def _hue_vec(h):
    """色相 -> 零和 RGB 偏移方向：三相余弦相加为 0，再加亮度投影修正。"""
    a = np.deg2rad(float(h))
    v = np.array([np.cos(a), np.cos(a - 2.094395),
                  np.cos(a - 4.188790)], dtype=np.float32)
    v -= LUMA * float(v @ LUMA) / float(LUMA @ LUMA)
    return v


_GRADE_N = 1024


def _grade_table(a, bl, sh_h, sh_s, sh_l, mid_h, mid_s, mid_l,
                 hi_h, hi_s, hi_l, gl_h, gl_s, gl_l):
    """亮度 -> RGB 染色偏移；权重只跟亮度有关，压成一维表比逐像素算快。

    bl（Blending）与 a（Balance）是两个独立参数：
      a  移动三段的分割位置（阴影区多大 / 高光区多大）
      bl 控制三段之间的过渡软硬：0 = 旧的三段硬权重，100 = 按亮度完全融合
    """
    lum = np.linspace(0.0, 1.0, _GRADE_N, dtype=np.float32)
    w_sh = 1.0 - _smooth(a * 0.45, a, lum)
    w_hi = _smooth(min(a * 1.15, 0.95), min(a * 2.0, 1.0), lum)
    w_mid = np.clip(1.0 - w_sh - w_hi, 0.0, 1.0)
    if bl > 0.0:
        w_sh = w_sh * (1.0 - bl) + bl * (1.0 - lum)
        w_hi = w_hi * (1.0 - bl) + bl * lum
        w_mid = np.clip(1.0 - w_sh - w_hi, 0.0, 1.0)
    tab = (w_sh[:, None] * (_hue_vec(sh_h) * (sh_s / 100.0 * 0.18))
           + w_mid[:, None] * (_hue_vec(mid_h) * (mid_s / 100.0 * 0.18))
           + w_hi[:, None] * (_hue_vec(hi_h) * (hi_s / 100.0 * 0.18)))
    if gl_s:
        # 全局轮：不分亮度区，整幅图统一偏色
        tab = tab + _hue_vec(gl_h) * (gl_s / 100.0 * 0.18)
    # Lum：三段与全局各带一个亮度偏移（零和色相向量本身不带亮度）
    if sh_l or mid_l or hi_l or gl_l:
        tab = tab + (w_sh * (sh_l / 100.0 * 0.15)
                     + w_mid * (mid_l / 100.0 * 0.15)
                     + w_hi * (hi_l / 100.0 * 0.15)
                     + gl_l / 100.0 * 0.15)[:, None]
    return np.ascontiguousarray(tab, dtype=np.float32)


def _color_grade(x, p):
    sh_s, mid_s, hi_s = (p.get("grade_sh_s", 0.0), p.get("grade_mid_s", 0.0),
                         p.get("grade_hi_s", 0.0))
    gl_s = p.get("grade_global_s", 0.0)
    sh_l, mid_l, hi_l = (p.get("grade_sh_l", 0.0), p.get("grade_mid_l", 0.0),
                         p.get("grade_hi_l", 0.0))
    gl_l = p.get("grade_global_l", 0.0)
    if not (sh_s or mid_s or hi_s or gl_s
            or sh_l or mid_l or hi_l or gl_l):
        return x
    sh_h, mid_h, hi_h = (p.get("grade_sh_h", 0.0), p.get("grade_mid_h", 0.0),
                         p.get("grade_hi_h", 0.0))
    gl_h = p.get("grade_global_h", 0.0)
    # 平衡右移 = 阴影过渡提前结束、高光区更大
    a = float(np.clip(0.46 - p.get("grade_balance", 0.0) / 100.0 * 0.12,
                      0.18, 0.78))
    bl = float(np.clip(p.get("grade_blending", 0.0) or 0.0, 0.0, 100.0)) / 100.0
    key = ("grade", round(a, 4), round(bl, 4),
           round(sh_h, 1), round(sh_s, 2), round(sh_l, 2),
           round(mid_h, 1), round(mid_s, 2), round(mid_l, 2),
           round(hi_h, 1), round(hi_s, 2), round(hi_l, 2),
           round(gl_h, 1), round(gl_s, 2), round(gl_l, 2))
    tab = _cached(key, lambda: _grade_table(
        a, bl, sh_h, sh_s, sh_l, mid_h, mid_s, mid_l,
        hi_h, hi_s, hi_l, gl_h, gl_s, gl_l))
    lum = np.clip(x @ LUMA, 0.0, 1.0)
    idx = np.clip(lum * (_GRADE_N - 1) + 0.5, 0, _GRADE_N - 1).astype(np.int32)
    idx = idx[..., None] * 3 + np.arange(3, dtype=np.int32)
    return np.clip(x + tab.ravel()[idx], 0.0, 1.0)


# ---- 自动校正 ----
_PROBE_LONG = 256      # 自动校正的测量与验证图长边
_TARGET_MED = 0.46     # 目标中间调亮度
_CLIP_TOL = 0.002      # 允许比原图多贴边的像素比例（0.2%）
_MAX_EV = 1.5          # 单帧自动曝光上限：夜景不是靠硬拉曝光救的
_SCALES = (1.0, 0.7, 0.5, 0.35, 0.2, 0.0)   # 收缩档位，0 即不动
# 验收不过时的搜索顺序：先只收白色/对比/黑位这几项「风格」，把曝光这个真正的
# 修正保住；风格收干净了还不行，才连曝光一起往下收。最后一档 (0, 0) 什么都不动，
# 必然通过，所以循环一定有结果。
_SEARCH = [(1.0, t) for t in _SCALES] + [(e, e) for e in _SCALES[1:]]


def _probe(img):
    """缩到长边 _PROBE_LONG 的副本：自动校正的统计和验证都在它上面做。"""
    x = np.clip(np.asarray(img, dtype=np.float32), 0.0, 1.0)
    step = max(1, int(max(x.shape[0], x.shape[1]) // _PROBE_LONG))
    return x if step == 1 else x[::step, ::step]


def _clip_fracs(lum):
    """(过曝比例, 死黑比例)：直方图两端贴边才算，不是看百分位够不够高。"""
    n = max(1, int(lum.size))
    return (float((lum >= 0.995).sum()) / n, float((lum <= 0.005).sum()) / n)


def _auto_render(x, q):
    """把建议参数真的算一遍（逐点链，跳过锐化降噪），返回 (过曝, 死黑, 中间调)。

    验证必须走真实管线：影调四段是乘加混在一起的，用百分位估算结果会低估
    高光贴边的比例，自动校正就会在该收手的时候没收手。
    """
    out = _core(x, {**defaults(), **q}, for_lut=True, fast=True)
    lum = np.clip(out, 0.0, 1.0) @ LUMA
    hi, lo = _clip_fracs(lum)
    return hi, lo, float(np.median(lum))


def _auto_safe(cur, ref):
    """保护项的底线：不许把任何一端推贴边。"""
    return cur[0] <= ref[0] + _CLIP_TOL and cur[1] <= ref[1] + _CLIP_TOL


def _auto_better(cur, ref):
    """激进项的底线：贴边不许变多，而且中间调要真的更靠近目标。"""
    return (_auto_safe(cur, ref)
            and abs(cur[2] - _TARGET_MED) <= abs(ref[2] - _TARGET_MED) + 1e-3)


def auto_params(img):
    """稳健版自动白平衡 + 自动影调，返回建议参数（不直接改 p）。

    先按灰度世界和 1/50/99 百分位给一版提案，再把提案真的算一遍来验收：
    过曝和死黑的像素比例不许比原图更多，中间调必须比改之前更接近目标亮度。
    不达标就按档位收缩——先收白色/对比/黑位这几项「风格」，把曝光这个真正的
    修正留住；风格收干净还不行才连曝光一起收，收到 0 就只剩保护项（压高光、
    提阴影、白平衡）。所以本来偏亮的雪景不会被压成灰、夜景不会被硬拉到中灰后
    爆掉路灯，已经曝好的图干脆不动。
    """
    x = _probe(img)
    lum0 = np.clip(x @ LUMA, 0.0, 1.0)
    ref = _clip_fracs(lum0) + (float(np.median(lum0)),)
    p50 = float(np.percentile(lum0, 50))

    ev = float(np.clip(np.log2(_TARGET_MED / max(p50, 1e-3)),
                       -_MAX_EV, _MAX_EV))
    if abs(ev) < 0.1:
        ev = 0.0

    l2 = np.clip(lum0 * (2.0 ** ev), 0.0, 1.0)
    q1, q99 = (float(v) for v in np.percentile(l2, (1, 99)))
    protect = {"highlights": float(np.clip(-(q99 - 0.94) * 350.0, -70.0, 0.0)),
               "shadows": float(np.clip((0.12 - q1) * 260.0, 0.0, 60.0))}
    risky = {"exposure": ev,
             "whites": float(np.clip((0.94 - q99) * 250.0, 0.0, 35.0)),
             "contrast": float(np.clip((0.75 - (q99 - q1)) * 80.0, -15.0, 30.0)),
             # 黑位只在暗端本来就浮得太高时往下压；已经贴死黑的不能再压
             "blacks": float(np.clip(-(q1 - 0.07) * 220.0, -20.0, 0.0))}
    mx, mn = x.max(axis=-1), x.min(axis=-1)
    sat = (mx - mn) / np.maximum(mx, 1e-4)
    sel = (lum0 > 0.08) & (lum0 < 0.92) & (sat < 0.35)
    t = u = 0.0
    if int(sel.sum()) > x.shape[0] * x.shape[1] * 0.02:
        r, g, b = (float(np.median(x[..., c][sel])) for c in range(3))
        g = max(g, 1e-4)
        t = float(np.clip((b / g - r / g) / 0.60 * 100.0, -80.0, 80.0))
        green = g / max((r * b) ** 0.5, 1e-4)
        u = float(np.clip((green - 1.0) / 0.28 * 100.0, -60.0, 60.0))
    wb = {"temperature": t, "tint": u}

    keep = dict(wb)
    if _auto_safe(_auto_render(x, {**protect, **wb}), ref):
        keep.update(protect)
    ref2 = _auto_render(x, keep)
    for fe, ft in _SEARCH:
        q = {**keep, "exposure": risky["exposure"] * fe,
             **{k: risky[k] * ft for k in ("whites", "contrast", "blacks")}}
        if _auto_better(_auto_render(x, q), ref2):
            break
    return {"exposure": round(float(q["exposure"]), 2),
            "contrast": round(float(q["contrast"])),
            "highlights": round(float(q.get("highlights", 0.0))),
            "shadows": round(float(q.get("shadows", 0.0))),
            "whites": round(float(q["whites"])),
            "blacks": round(float(q["blacks"])),
            "temperature": round(t), "tint": round(u)}


def _core(x, p, for_lut=False, fast=False, local=False):
    """影调 + 色彩主链：从白平衡到色阶/曲线，不含镜头校正、颗粒、晕影与蒙版。

    local=True 是给局部蒙版复用同一条链的：细节类（降噪、锐化）和全局专属
    （单色）在蒙版里没有意义，跳过；拖动草图时连清晰度/纹理也算不过来。
    """
    # 降噪在校正之前：先压掉噪点，后面拉曝光/清晰度才不会把它一起放大
    nl, nc = p.get("nr_luma", 0.0), p.get("nr_color", 0.0)
    if not for_lut and not fast and not local and (nl or nc):
        x = _denoise(x, nl, nc, p.get("nr_luma_detail", 0.0),
                     p.get("nr_luma_contrast", 0.0),
                     p.get("nr_color_smooth", 50.0),
                     p.get("nr_multiscale", 0.0),
                     p.get("nr_sigma_manual", 0.0))

    # 色温 / 色调 + 曝光：线性光里一次完成（亮度归一化，明暗不漂）
    x = _wb_exposure(x, p["temperature"] / 100.0, p["tint"] / 100.0,
                     float(p["exposure"]), chan_ev(p))

    # 曝光高光肩部（对齐 LR UniformsExposure 的 rolloff；knee/len 需拟合）：
    # 有理肩部 t=0 处斜率 1 → C1 连续；线性域亮度比值缩放，保色相。
    # 与 Rust develop.rs::core 的曝光段逐式对应。
    krl = p.get("hl_rolloff", 0.0) / 100.0
    if krl != 0.0:
        xl = _srgb_to_lin(x)
        y = xl @ LUMA
        tt = np.maximum(y - np.float32(0.75), np.float32(0.0))
        yy = y + np.float32(krl) * tt / (np.float32(1.0) + tt / np.float32(0.40))
        s = yy / np.maximum(y, np.float32(1e-6))
        x = _lin_to_srgb(xl * s[..., None])

    # 对比度（smoothstep 混合：中间调变陡，两端压缩）
    # 曲线只压 0~1，>1 的高光保留原值，避免过曝部分被压成负数
    # contrast_linear>0 时把完整映射搬进线性光域再回编码域取增量，与旧
    # gamma 域增量按 k 混合（E3/推断）。与 Rust develop.rs::core 逐式对应。
    c = p["contrast"] / 100.0
    ck = p.get("contrast_linear", 0.0) / 100.0
    if c != 0.0 and ck > 0.0:
        xc = np.minimum(x, 1.0)
        xl = _srgb_to_lin(xc)
        s_lin = _contrast_s_lin(xl)
        delta_lin = _lin_to_srgb(xl + np.float32(c) * (s_lin - xl)) - xc
        delta_gam = np.float32(c) * (xc * xc * (3.0 - 2.0 * xc) - xc)
        x = x + delta_gam * np.float32(1.0 - ck) + delta_lin * np.float32(ck)
    elif c != 0.0:
        xc = np.minimum(x, 1.0)
        s = xc * xc * (3.0 - 2.0 * xc)
        x = x + c * (s - xc)

    # 去朦胧 / 加雾（逐点，可进 LUT）：正=下沉黑位、提升通透，负=抬黑加雾
    dz = p.get("dehaze", 0.0) / 100.0
    if dz > 0.0:
        v = dz * 0.25
        x = (x - v) / (1.0 - v)
    elif dz < 0.0:
        v = -dz
        x = x * (1.0 - v * 0.20) + v * 0.07

    # 暗通道先验去雾（He DCP + 大气散射）：空间感知的去雾，叠在旧逐点口径
    # 之后。fast（草图）/local 跳过——与降噪/锐化同口径；for_lut 跳过（空间
    # 运算不进 LUT）。dehaze_dark=0 整段跳过 = 旧路径逐位一致。与 Rust
    # develop.rs::core 的插入点一致（旧去雾段之后）。
    dd = p.get("dehaze_dark", 0.0)
    if not for_lut and not fast and not local and dd:
        x = _dehaze_dcp(x, float(dd) / 100.0,
                        float(p.get("dehaze_contrast", 0.0)) / 100.0)

    # 高光 / 阴影 / 白色 / 黑色
    # 四段都是"按亮度权重乘/加"，旧写法各扫一遍全图；合成为 x*fac+add
    # 一次扫完（四段全开时约 150ms → 60ms）。累加顺序与逐段写完全一致。
    # black_soft/white_soft>0 时把段末的 [0,4] 硬钳换成抛物线软拐点
    # （对齐 LR plusWhite/BlackClip 的硬软分离；宽度需拟合）：
    # 输出恒落 [0,1]、下游契约不变。与 Rust develop.rs::core 逐式对应。
    s_b = (0.08 + 0.17 * (p.get("black_soft", 0.0) / 100.0)
           if p.get("black_soft", 0.0) else 0.0)
    s_w = (0.08 + 0.17 * (p.get("white_soft", 0.0) / 100.0)
           if p.get("white_soft", 0.0) else 0.0)
    if any(p[k] for k in ("highlights", "shadows", "whites", "blacks")) \
            or s_b or s_w:
        lum = x @ LUMA
        fac = np.ones_like(lum)
        add = np.zeros_like(lum)
        a = p["highlights"] / 100.0
        if a:
            fac *= 1.0 + a * 0.5 * _smooth(0.45, 1.0, lum)
        a = p["shadows"] / 100.0
        if a:
            k = a * 0.45 * _smooth(0.55, 0.0, lum)
            fac *= 1.0 - k
            add = add * (1.0 - k) + k * 1.2
        a = p["whites"] / 100.0
        if a:
            k = a * 0.5 * (_smooth(0.60, 1.0, lum) ** 2)
            fac *= 1.0 - k
            add = add * (1.0 - k) + k
        a = p["blacks"] / 100.0
        if a:
            add += a * 0.35 * (_smooth(0.40, 0.0, lum) ** 2)
        x = x * fac[..., None] + add[..., None]
        if s_b == 0.0 and s_w == 0.0:
            x = np.clip(x, 0.0, 4.0)
        else:
            # 单侧关闭时保持旧硬钳的对应半边（min 4 / max 0）
            x = _soft_clip_high(x, s_w) if s_w > 0.0 else np.minimum(x, 4.0)
            x = _soft_clip_low(x, s_b) if s_b > 0.0 else np.maximum(x, 0.0)

    # 锐化（capture sharpening，空间效果）
    if not for_lut and not fast and not local and p.get("sharp_amount", 0.0):
        x = _sharpen(x, p["sharp_amount"], p.get("sharp_radius", 1.0),
                     p.get("sharp_detail", 25.0),
                     p.get("sharp_masking", 0.0),
                     p.get("sharp_protect", 0.0))

    # 清晰度（大半径、边缘感知的局部对比，空间效果）
    if not for_lut and p["clarity"] and not (local and fast):
        H, W = x.shape[:2]
        x = _local_contrast(x, p["clarity"] / 100.0 * 1.6,
                            max(2, int(min(H, W) / 60)), 0.02, 1.0)

    # 纹理（小半径局部对比，空间效果）
    tx = p.get("texture", 0.0)
    if not for_lut and tx and not (local and fast):
        H, W = x.shape[:2]
        x = _local_contrast(x, tx / 100.0, max(1, int(min(H, W) / 300)),
                            0.004, 1.0)

    # 自然饱和度（低饱和像素提升更多）→ 饱和度
    # 两段都以同一个 lum 为轴，系数可以先乘起来，少扫一遍全图。
    v, s = p["vibrance"] / 100.0, p["saturation"] / 100.0
    if v != 0.0 or s != 0.0:
        x = np.clip(x, 0.0, 1.0)
        lum = x @ LUMA
        f = np.float32(1.0 + s)
        if v:
            f = (f * (1.0 + v * (1.0 - _chan_range(x)) * 1.3))[..., None]
        x = lum[..., None] + (x - lum[..., None]) * f
        x = np.clip(x, 0.0, 1.0)

    # 色彩分级：阴影 / 中间调 / 高光三段染色（纯颜色运算，可进 LUT）
    x = _color_grade(x, p)

    # HSL：8 色相带（色相偏移 / 饱和度 / 明亮度）
    if any(p["hsl_h"]) or any(p["hsl_s"]) or any(p["hsl_l"]):
        x = np.clip(x, 0.0, 1.0)
        shp = x.shape
        src = x if x.ndim == 3 else x.reshape(-1, 1, 3)
        hsv = cv2.cvtColor(np.ascontiguousarray(src), cv2.COLOR_RGB2HSV)
        h = hsv[..., 0]
        key = ("hsl", tuple(p["hsl_h"]), tuple(p["hsl_s"]), tuple(p["hsl_l"]))
        ts, tf, tl = _cached(key, lambda: _hsl_tables(p))
        idx = (h * (_HUE_BINS / 360.0)).astype(np.int32) & (_HUE_BINS - 1)
        if ts is not None:
            h = h + np.take(ts, idx)
            np.subtract(h, 360.0, out=h, where=h >= 360.0)
            np.add(h, 360.0, out=h, where=h < 0.0)
            hsv[..., 0] = h
        if tf is not None:
            sv = hsv[..., 1] * (1.0 + np.take(tf, idx))
            np.clip(sv, 0.0, 1.0, out=sv)
            hsv[..., 1] = sv
        if tl is not None:
            vv = hsv[..., 2] * (1.0 + np.take(tl, idx))
            np.clip(vv, 0.0, 1.0, out=vv)
            hsv[..., 2] = vv
        x = cv2.cvtColor(hsv, cv2.COLOR_HSV2RGB).reshape(shp)

    # 单色 + 色相亮度混合（全局专属，蒙版里用饱和度去色就够了）
    if not local:
        x = _mono(x, p)

    # 色阶 + 参数曲线 + 曲线：合成一张 16bit 表，一次查完
    if not _curves_default(p) or _levels_active(p) or _par_active(p):
        # 曲线 HDR 尾部（对齐 LR fExtension_LumaHDR 的「延伸而非截断」；
        # 斜率混合需拟合）：x>1 的输入按「曲线端点值 + PCHIP 末端斜率延伸」
        # 处理，输出仍 clamp ≤1；x∈[0,1] 主路径逐位不动。与 Rust
        # lut.rs::tone_curve 逐式对应。
        k_hdr = p.get("curve_hdr_tail", 0.0) / 100.0
        if k_hdr > 0.0:
            flat = np.clip(x, 0.0, 1.0)
            shp = x.shape
            q = np.rint(flat * 65535.0).astype(np.uint16).reshape(-1, 1, 3)
            key = ("point", tuple(map(tuple, p["curve_rgb"])),
                   tuple(map(tuple, p["curve_r"])), tuple(map(tuple, p["curve_g"])),
                   tuple(map(tuple, p["curve_b"])),
                   round(float(p.get("lv_black", 0.0)), 2),
                   round(float(p.get("lv_white", 255.0)), 2),
                   round(float(p.get("lv_gamma", 1.0)), 3),
                   round(float(p.get("lv_out_black", 0.0)), 2),
                   round(float(p.get("lv_out_white", 255.0)), 2),
                   round(float(p.get("par_shadows", 0.0) or 0.0), 1),
                   round(float(p.get("par_darks", 0.0) or 0.0), 1),
                   round(float(p.get("par_lights", 0.0) or 0.0), 1),
                   round(float(p.get("par_highlights", 0.0) or 0.0), 1),
                   round(float(p.get("par_split1", 25.0) or 0.0), 1),
                   round(float(p.get("par_split2", 50.0) or 0.0), 1),
                   round(float(p.get("par_split3", 75.0) or 0.0), 1))
            lut = _cached(key, lambda: _curve_lut(p))
            # 末端斜率（0..255 空间）+ k*0.6 混合；端点斜率 0 时尾部自然水平
            pts = sorted((float(px_), float(py)) for px_, py in p["curve_rgb"])
            if len(pts) >= 2:
                base_slope = (pts[-1][1] - pts[-2][1]) / max(pts[-1][0] - pts[-2][0], 1e-9)
            else:
                base_slope = 0.0
            slope = np.float32(base_slope / 255.0 + k_hdr * 0.6)
            y1 = lut[-1, 0, :].astype(np.float32) * np.float32(1.0 / 65535.0)
            tail = np.clip(y1 + (x - 1.0) * slope, 0.0, 1.0)
            tab = (cv2.LUT(q, lut).astype(np.float32).reshape(shp)
                   * (1.0 / 65535.0))
            x = np.where(x > 1.0, tail, tab)
        else:
            x = np.clip(x, 0.0, 1.0)
            shp = x.shape
            q = np.rint(x * 65535.0).astype(np.uint16).reshape(-1, 1, 3)
            key = ("point", tuple(map(tuple, p["curve_rgb"])),
                   tuple(map(tuple, p["curve_r"])), tuple(map(tuple, p["curve_g"])),
                   tuple(map(tuple, p["curve_b"])),
                   round(float(p.get("lv_black", 0.0)), 2),
                   round(float(p.get("lv_white", 255.0)), 2),
                   round(float(p.get("lv_gamma", 1.0)), 3),
                   round(float(p.get("lv_out_black", 0.0)), 2),
                   round(float(p.get("lv_out_white", 255.0)), 2),
                   round(float(p.get("par_shadows", 0.0) or 0.0), 1),
                   round(float(p.get("par_darks", 0.0) or 0.0), 1),
                   round(float(p.get("par_lights", 0.0) or 0.0), 1),
                   round(float(p.get("par_highlights", 0.0) or 0.0), 1),
                   round(float(p.get("par_split1", 25.0) or 0.0), 1),
                   round(float(p.get("par_split2", 50.0) or 0.0), 1),
                   round(float(p.get("par_split3", 75.0) or 0.0), 1))
            lut = _cached(key, lambda: _curve_lut(p))
            x = (cv2.LUT(q, lut).astype(np.float32).reshape(shp)
                 * (1.0 / 65535.0))

    return x


def apply(img, p, for_lut=False, fast=False):
    """主流水线。img: float32 (…,3) RGB [0,1]。返回 float32 [0,1]。

    fast=True 是拖动时给低分辨率草图用的：跳过降噪与锐化这类在缩略图上
    几乎看不出、却最吃时间的细节运算；松手后的精修会补上。
    for_lut=True 只留逐点运算（镜头校正、颗粒、晕影、蒙版全跳），
    用来生成 .cube，保证视频导出与预览一致。
    """
    x = np.clip(np.asarray(img, dtype=np.float32), 0.0, 1.0)

    # 镜头校正排在最前：几何与紫边要先修掉，后面的局部对比才不会跟着畸变跑
    if not for_lut:
        x = _lens(x, p)

    x0 = x
    x = _core(x0, p, for_lut=for_lut, fast=fast)

    # 局部蒙版：拿同一张原图叠上该组的增量再算一遍，按组权重混回来。
    # 亮度范围蒙版要拿原图亮度，只算一次，混色与空间项共用。
    if not for_lut:
        ms = _mask_active(p)
        if ms:
            lum0 = (np.clip(x0 @ LUMA, 0.0, 1.0)
                    if masks_use_range(p) else None)
            x = _blend_masks(x, x0, p, ms, fast, lum0)
            if not fast:
                x = _mask_spatial(x, p, lum0)

    # 颗粒（空间噪声，不进 LUT；预览与静态导出用固定种子保证可复现）
    # grain_lum 是亮度调制（对齐 LR shadowScale/highlightScale）：
    # 暗部增强、高光减弱，负值反转；k=0 走旧路径逐位一致。与 Rust
    # spatial.rs::grain 逐式对应。系数是设计选择而非拟合值（没有参考图）：
    # 端点为拉满时暗部 ×2.5、高光 ×0.3，0.5 档为 ×1.75 / ×0.65。
    gr = p.get("grain", 0.0)
    if not for_lut and gr:
        H, W = x.shape[:2]
        amp = (gr / 100.0) * 0.075
        n = _noise(H, W, p.get("grain_size", 1.0), p.get("grain_freq", 1.0))
        kl = p.get("grain_lum", 0.0) / 100.0
        if kl != 0.0:
            y = np.clip(x @ LUMA, 0.0, 1.0)
            dark = 1.0 + np.float32(kl) * 1.5 * _smooth(0.55, 0.0, y)
            light = 1.0 - np.float32(kl) * 0.7 * _smooth(0.45, 1.0, y)
            x = x + (np.float32(amp) * dark * light)[..., None] * n
        else:
            x = x + np.float32(amp) * n

    # 晕影（空间效果）：衰减场按尺寸缓存，调色时只重算乘法。
    # 高光保护要像素亮度，只有开了这个参数才算一次亮度图。
    if not for_lut and (p.get("vig_amount", p.get("vignette", 0.0)) or 0.0):
        H, W = x.shape[:2]
        lum = (np.clip(x @ LUMA, 0.0, 1.0)
               if p.get("vig_hl_contrast") else None)
        m = vignette_mask(W, H, p, lum=lum)
        if m is not None:
            x = x * m[..., None]

    return np.clip(x, 0.0, 1.0)


def write_cube(path, p, size=65):
    """生成 3D LUT (.cube)，红色分量变化最快（标准顺序）。"""
    ax = np.linspace(0.0, 1.0, size, dtype=np.float32)
    lat = np.empty((size, size, size, 3), dtype=np.float32)
    lat[..., 0] = ax.reshape(1, 1, -1)   # r：最快
    lat[..., 1] = ax.reshape(1, -1, 1)   # g：次之
    lat[..., 2] = ax.reshape(-1, 1, 1)   # b：最慢
    out = np.clip(apply(lat, p, for_lut=True), 0.0, 1.0).reshape(-1, 3)
    with open(path, "w", encoding="ascii", newline="\n") as f:
        f.write('# ColorTool 3D LUT\nTITLE "grade"\n'
                f'LUT_3D_SIZE {size}\n'
                'DOMAIN_MIN 0.0 0.0 0.0\nDOMAIN_MAX 1.0 1.0 1.0\n')
        np.savetxt(f, out, fmt="%.6f")
