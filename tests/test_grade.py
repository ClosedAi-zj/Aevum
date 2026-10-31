# -*- coding: utf-8 -*-
"""调色核心数值回归：查表化的三段（HSL / 曲线 / 晕影）必须与逐像素旧算法一致。

acceptance/reference/grade.py 为了拖动流畅把这三段换成了查表与缓存，等价性只能靠对照实现来锁，
所以这里逐字保留旧写法作为参照。容差按 8bit 台阶（1/255）给：新版与旧版各自都会
量化到 256 级，落在台阶边界上的像素允许差一档。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_grade.py
"""
import os
import sys
import time

import cv2
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "acceptance"))

from reference import grade as gc            # noqa: E402

LSB = 1.0 / 255.0
FAILS = []


def check(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}"
          + (f"  {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


# ---------------------------------------------------------------- 旧算法参照
def ref_hsl(x, p):
    """逐像素 8 遍色相带（旧写法）。"""
    x = np.clip(x, 0.0, 1.0)
    shp = x.shape
    hsv = cv2.cvtColor(np.ascontiguousarray(x.reshape(-1, 1, 3)),
                       cv2.COLOR_RGB2HSV)
    h = hsv[..., 0]
    shift = np.zeros_like(h)
    sfac = np.zeros_like(h)
    lfac = np.zeros_like(h)
    for i in range(8):
        hs, ss, ls = p["hsl_h"][i], p["hsl_s"][i], p["hsl_l"][i]
        if not (hs or ss or ls):
            continue
        d = np.abs((h - gc.HSL_CENTERS[i] + 180.0) % 360.0 - 180.0)
        w = gc._smooth(50.0, 18.0, d)
        if hs:
            shift += (hs / 100.0) * 40.0 * w
        if ss:
            sfac += (ss / 100.0) * w
        if ls:
            lfac += (ls / 100.0) * 0.6 * w
    if shift.any():
        hsv[..., 0] = (hsv[..., 0] + shift) % 360.0
    if sfac.any():
        hsv[..., 1] = np.clip(hsv[..., 1] * (1.0 + sfac), 0.0, 1.0)
    if lfac.any():
        hsv[..., 2] = np.clip(hsv[..., 2] * (1.0 + lfac), 0.0, 1.0)
    return cv2.cvtColor(hsv, cv2.COLOR_HSV2RGB).reshape(shp)


def ref_curve(x, p):
    """分通道曲线后接主曲线，用 16bit 级表逼近连续函数。

    旧参照本身量化到 8bit，已经没法验证"去掉 8bit 台阶"这件事；
    这里改用 65536 级表，误差只剩输入索引的半步。
    """
    x = np.clip(x, 0.0, 1.0)
    N = 1 << 16
    luts = [gc.pchip_lut(p[k], N) for k in ("curve_r", "curve_g", "curve_b")]
    master = gc.pchip_lut(p["curve_rgb"], N)
    idx = np.clip(np.rint(x * (N - 1)), 0, N - 1).astype(np.int32)
    for ch in range(3):
        vv = luts[ch][idx[..., ch]]
        idx[..., ch] = np.clip(np.rint(vv / 255.0 * (N - 1)), 0, N - 1)
    return master[idx].astype(np.float32) / 255.0


def ref_vignette(x, amount):
    """每次重算 mgrid 的晕影（旧写法）。"""
    H, W = x.shape[:2]
    yy, xx = np.mgrid[0:H, 0:W].astype(np.float32)
    nx = (xx - (W - 1) / 2) / (W / 2)
    ny = (yy - (H - 1) / 2) / (H / 2)
    r = np.sqrt(nx * nx + ny * ny) / np.sqrt(2.0)
    a = float(amount) / 100.0
    mask = (1.0 - a * gc._smooth(0.45, 1.05, r)) if a >= 0 else \
           (1.0 + (-a) * gc._smooth(0.45, 1.05, r))
    return x * np.clip(mask, 0.0, 2.0).astype(np.float32)[..., None]


# ---------------------------------------------------------------- 用例
def param_sets():
    d = gc.defaults()
    r = np.random.default_rng(11)
    sets = {"预设·原始": d}
    for name in gc.PRESETS:
        if name != "原始":
            sets["预设·" + name] = gc.preset(name)
    q = dict(d)
    q["hsl_h"] = list(np.round(r.uniform(-100, 100, 8), 1))
    q["hsl_s"] = list(np.round(r.uniform(-100, 100, 8), 1))
    q["hsl_l"] = list(np.round(r.uniform(-100, 100, 8), 1))
    sets["随机 HSL"] = q
    c = dict(d)
    c["curve_rgb"] = [[0, 14], [48, 40], [128, 138], [192, 208], [255, 250]]
    c["curve_r"] = [[0, 8], [128, 140], [255, 255]]
    c["curve_b"] = [[0, 0], [64, 50], [255, 240]]
    sets["随机曲线"] = c
    sets["极值 HSL"] = {**d, "hsl_h": [100] * 8, "hsl_s": [100] * 8,
                        "hsl_l": [-100] * 8}
    sets["极值曲线"] = {**d, "curve_rgb": [[0, 255], [255, 0]]}
    return sets


def test_hsl(img):
    d = gc.defaults()
    worst = 0.0
    for name, p in param_sets().items():
        if not (any(p["hsl_h"]) or any(p["hsl_s"]) or any(p["hsl_l"])):
            continue
        only = {**d, "hsl_h": p["hsl_h"], "hsl_s": p["hsl_s"],
                "hsl_l": p["hsl_l"]}
        a = ref_hsl(img, only)
        b = gc.apply(img, only)
        worst = max(worst, float(np.abs(a - b).max()))
    check("HSL 查表与逐像素旧算法一致", worst < 0.2 * LSB,
          f"最大差 {worst:.2e}（{worst / LSB:.2f} 个 8bit 台阶）")


def test_curve(img):
    d = gc.defaults()
    worst = 0.0
    for name, p in param_sets().items():
        if gc._curves_default(p):
            continue
        only = {**d, **{k: p[k] for k in
                        ("curve_rgb", "curve_r", "curve_g", "curve_b")}}
        a = ref_curve(img, only)
        b = gc.apply(img, only)
        worst = max(worst, float(np.abs(a - b).max()))
    check("曲线合表与高精度参照一致", worst < 0.02 * LSB,
          f"最大差 {worst:.2e}（{worst / LSB:.2f} 个台阶）")

    # 8bit 合表最多只有 256 个不同输出；16bit 要能保留暗部渐变的细台阶
    ramp = np.linspace(0.0, 1.0, 4096, dtype=np.float32)
    band = np.repeat(np.repeat(ramp[:, None, None], 3, axis=2), 16, axis=1)
    p = {**gc.defaults(),
         "curve_rgb": [[0, 0], [64, 80], [192, 210], [255, 255]]}
    out = gc.apply(band, p)[:, 0, 0]
    levels = len(np.unique(np.rint(out * 65535).astype(np.int32)))
    check("曲线保留 16bit 输出精度", levels > 3000, f"{levels} 个不同值")


def test_vignette(img):
    worst = 0.0
    for amt in (-100, -60, -10, 0.0, 12, 45, 100):
        p = {**gc.defaults(), "vignette": amt}
        a = np.clip(ref_vignette(np.clip(img, 0, 1), amt), 0.0, 1.0)
        b = gc.apply(img, p)
        worst = max(worst, float(np.abs(a - b).max()))
    check("晕影缓存半径与旧 mgrid 一致", worst < 0.5 * LSB,
          f"最大差 {worst:.2e}")


def hi_pass(im):
    g = im[..., :3].mean(axis=2)
    k = (g[1:-1, 1:-1] * 4 - g[:-2, 1:-1] - g[2:, 1:-1]
         - g[1:-1, :-2] - g[1:-1, 2:])
    return float(np.abs(k).mean())


def test_new_effects(img):
    """线性光白平衡、锐化/降噪、色彩分级、自动校正的行为回归。"""
    d = gc.defaults()
    rng = np.random.default_rng(21)

    gray = np.full((96, 128, 3), 0.5, dtype=np.float32)
    l0 = float((gray @ gc.LUMA).mean())
    l1 = float((gc.apply(gray, {**d, "temperature": 80}) @ gc.LUMA).mean())
    check("白平衡保持整体亮度", abs(l1 - l0) < 0.01, f"{l0:.4f} -> {l1:.4f}")

    warm = gc.apply(img, {**d, "temperature": 80})
    check("色温方向：暖 = 红升蓝降",
          warm[..., 0].mean() > img[..., 0].mean() + 0.02
          and warm[..., 2].mean() < img[..., 2].mean() - 0.02,
          f"ΔR={warm[...,0].mean()-img[...,0].mean():+.3f} "
          f"ΔB={warm[...,2].mean()-img[...,2].mean():+.3f}")

    sharp = {**d, "sharp_amount": 120}
    check("锐化提升高频细节", hi_pass(gc.apply(img, sharp)) > hi_pass(img) * 1.2,
          f"{hi_pass(img):.3f} -> {hi_pass(gc.apply(img, sharp)):.3f}")
    check("草图跳过锐化", np.array_equal(gc.apply(img, sharp, fast=True), img))

    yy, xx = np.mgrid[0:240, 0:320].astype(np.float32)
    base = np.clip(np.stack([0.25 + 0.5 * xx / 320, 0.3 + 0.4 * yy / 240,
                             0.35 + 0.2 * np.sin(xx / 40)], -1), 0, 1)
    noisy = np.clip(base + rng.normal(0, 0.05, base.shape).astype(np.float32),
                    0, 1)
    dn = {**d, "nr_luma": 80, "nr_color": 70}
    check("降噪降低噪声",
          float(np.std(gc.apply(noisy, dn) - base))
          < float(np.std(noisy - base)) * 0.6,
          f"{np.std(noisy-base):.3f} -> {np.std(gc.apply(noisy,dn)-base):.3f}")
    check("草图跳过降噪", np.array_equal(gc.apply(noisy, dn, fast=True), noisy))

    y = np.linspace(0.0, 1.0, 200, dtype=np.float32)
    ramps = np.repeat(np.repeat(y[:, None, None], 3, axis=2), 120, axis=1)
    tint = {**d, "grade_sh_h": 240, "grade_sh_s": 100}
    g = gc.apply(ramps, tint)
    d_lo = float((g[0, :, 2] - ramps[0, :, 2]).mean())
    d_hi = float((g[-1, :, 2] - ramps[-1, :, 2]).mean())
    check("分级只染阴影", d_lo > 0.08 and abs(d_hi) < 1e-6,
          f"阴影 ΔB={d_lo:+.3f}，高光 ΔB={d_hi:+.3f}")
    bal = gc.apply(ramps, {**tint, "grade_balance": 100})
    mid = 70                                   # 亮度约 0.35
    check("分级平衡收窄阴影",
          float(bal[mid, :, 2].mean()) < float(g[mid, :, 2].mean()),
          f"{g[mid,:,2].mean():.3f} -> {bal[mid,:,2].mean():.3f}")
    check("分级饱和为 0 时恒等",
          np.array_equal(gc.apply(ramps, {**d, "grade_sh_h": 240,
                                         "grade_sh_s": 0}), ramps))

    # 全局轮：不分亮度区，暗部和高光都应被染上同样的蓝
    glob = gc.apply(ramps, {**d, "grade_global_h": 240,
                            "grade_global_s": 100})
    g_lo = float((glob[10, :, 2] - ramps[10, :, 2]).mean())
    g_hi = float((glob[160, :, 2] - ramps[160, :, 2]).mean())
    check("全局分级同时染暗部与高光", g_lo > 0.08 and g_hi > 0.08,
          f"暗 ΔB={g_lo:+.3f}，亮 ΔB={g_hi:+.3f}")
    check("全局饱和为 0 时恒等",
          np.array_equal(gc.apply(ramps, {**d, "grade_global_h": 240,
                                         "grade_global_s": 0}), ramps))

    # 参数曲线：4 区各推各的，默认恒等，输出单调
    par_id = gc.apply(ramps, {**d, "par_shadows": 60, "par_darks": -40,
                              "par_lights": 20, "par_highlights": -60})
    check("参数曲线默认恒等",
          np.array_equal(gc.apply(ramps, d), ramps))
    sh = gc.apply(ramps, {**d, "par_shadows": 100})
    hi = gc.apply(ramps, {**d, "par_highlights": 100})
    low_i, high_i = 10, 170
    check("阴影区先提暗部", float(sh[low_i, 0, 0] - ramps[low_i, 0, 0])
          > float(sh[high_i, 0, 0] - ramps[high_i, 0, 0]) + 0.1,
          f"暗 {sh[low_i,0,0]-ramps[low_i,0,0]:+.3f} / "
          f"亮 {sh[high_i,0,0]-ramps[high_i,0,0]:+.3f}")
    check("高光区先提亮部", float(hi[high_i, 0, 0] - ramps[high_i, 0, 0])
          > float(hi[low_i, 0, 0] - ramps[low_i, 0, 0]) + 0.1,
          f"暗 {hi[low_i,0,0]-ramps[low_i,0,0]:+.3f} / "
          f"亮 {hi[high_i,0,0]-ramps[high_i,0,0]:+.3f}")
    lum = par_id[..., 0].mean(axis=1)
    check("参数曲线输出不反转", bool(np.all(np.diff(lum) >= -1e-7)))
    moved = gc.apply(ramps, {**d, "par_shadows": 100, "par_split1": 60,
                             "par_split2": 70, "par_split3": 85})
    check("分割点扩大阴影区",
          float(moved[45, 0, 0] - ramps[45, 0, 0])
          > float(sh[45, 0, 0] - ramps[45, 0, 0]) + 0.02,
          f"{sh[45,0,0]-ramps[45,0,0]:+.3f} -> "
          f"{moved[45,0,0]-ramps[45,0,0]:+.3f}")
    lat0 = np.clip(rng.random((9, 9, 9, 3), dtype=np.float32), 0, 1)
    check("参数曲线进入 LUT",
          float(np.abs(gc.apply(lat0, {**d, "par_shadows": 50}, for_lut=True)
                       - gc.apply(lat0, {**d, "par_shadows": 50})).max())
          < 1e-6)

    # LUT 里要保留颜色类操作，跳过锐化/降噪
    only_color = {**d, "grade_sh_h": 240, "grade_sh_s": 60}
    lat = np.clip(rng.random((9, 9, 9, 3), dtype=np.float32), 0, 1)
    check("色彩分级进入 LUT",
          float(np.abs(gc.apply(lat, only_color, for_lut=True)
                       - gc.apply(lat, only_color)).max()) < 1e-6)


def test_basics(img):
    check("默认参数恒等",
          float(np.abs(gc.apply(img, gc.defaults()) - img).max()) < 1e-6)
    g = {**gc.defaults(), "grain": 50}
    check("颗粒固定种子可复现",
          np.array_equal(gc.apply(img, g), gc.apply(img, g)))
    keep = img.copy()
    gc.apply(img, {**gc.defaults(), "contrast": 30, "clarity": 20,
                   "vignette": 20, "hsl_s": [10] * 8})
    check("不就地修改输入图", np.array_equal(img, keep))
    out = gc.apply(img, {**gc.defaults(), "exposure": 2.0})
    check("输出范围 [0,1]", out.min() >= 0.0 and out.max() <= 1.0)


def test_lut_path():
    """write_cube 传的是 (n,n,n,3) 四维网格，cv2 绑定不接受，单独验。"""
    size = 8
    ax = np.linspace(0.0, 1.0, size, dtype=np.float32)
    lat = np.empty((size, size, size, 3), dtype=np.float32)
    lat[..., 0] = ax.reshape(1, 1, -1)
    lat[..., 1] = ax.reshape(1, -1, 1)
    lat[..., 2] = ax.reshape(-1, 1, 1)
    p = {**gc.defaults(), "hsl_s": [15] * 8, "hsl_h": [10, 0, 0, 0, 0, -10, 0, 0],
         "curve_rgb": [[0, 18], [64, 58], [192, 206], [255, 254]],
         "curve_r": [[0, 12], [128, 136], [255, 250]]}
    out = gc.apply(lat, p, for_lut=True)
    ref = ref_curve(ref_hsl(lat, p), p)
    check("4D LUT 网格路径", out.shape == lat.shape
          and float(np.abs(out - ref).max()) < 2 * LSB,
          f"最大差 {float(np.abs(out - ref).max()):.2e}")
    # 空间效果不该进 LUT
    sp = {**gc.defaults(), "clarity": 80, "texture": 80, "vignette": 80,
          "grain": 80, "sharp_amount": 80, "nr_luma": 80, "nr_color": 80}
    check("for_lut 跳过空间项",
          np.array_equal(gc.apply(lat, sp, for_lut=True),
                         np.clip(lat, 0, 1)))


def test_new_tools(img):
    """色阶 / 黑白混合 / 镜头校正 / 局部蒙版：新加的四面都要有可核对的行为。"""
    d = gc.defaults()

    # ---- 色阶：与手写公式一致（16bit 合表，参照不再预先量化到 8bit）----
    q = {**d, "lv_black": 40.0, "lv_white": 210.0, "lv_gamma": 1.4,
         "lv_out_black": 8.0, "lv_out_white": 245.0}
    lo, hi = q["lv_black"] / 255, q["lv_white"] / 255
    t = np.clip((np.clip(img, 0, 1) - lo) / (hi - lo), 0, 1) ** (1 / q["lv_gamma"])
    ref = (q["lv_out_black"] + t * (q["lv_out_white"] - q["lv_out_black"])) / 255
    got = gc.apply(img, q)
    check("色阶与公式一致", float(np.abs(got - ref).max()) < 0.1 * LSB,
          f"最大差 {float(np.abs(got - ref).max()) / LSB:.3f} 档")
    n = gc.apply(img, {**d, "lv_black": 0.0, "lv_white": 255.0, "lv_gamma": 1.0})
    check("色阶中性位恒等", float(np.abs(n - img).max()) < 1e-6)
    o = gc.apply(img, {**d, "lv_out_white": 128.0})
    check("色阶输出白场封顶", o.max() <= 128.0 / 255 + LSB,
          f"最亮 {o.max() * 255:.1f}")

    # ---- 黑白：单色 = 三通道相等，色相带权重按色相生效 ----
    m100 = gc.apply(img, {**d, "mono": 100.0})
    spread = float(np.abs(m100 - m100.mean(axis=2, keepdims=True)).max())
    lum = np.clip(img, 0, 1) @ gc.LUMA
    check("单色 100 转灰阶", spread < LSB
          and float(np.abs(m100[..., 0] - lum).max()) < 1.5 * LSB,
          f"通道差 {spread / LSB:.2f} 档")
    half = gc.apply(img, {**d, "mono": 50.0})
    check("单色 50 是彩灰各半", float(np.abs(
        half - 0.5 * (np.clip(img, 0, 1) + m100)).max()) < 1.5 * LSB)
    red = np.zeros((64, 64, 3), np.float32)
    red[..., 0] = 1.0
    blue = np.zeros((64, 64, 3), np.float32)
    blue[..., 2] = 1.0
    bump = {**d, "mono": 100.0}
    bump["mono_h"] = [100.0] + [0.0] * 7          # 只提红系
    base = {**d, "mono": 100.0}
    up_r = gc.apply(red, bump).mean()
    up_b = gc.apply(blue, bump).mean()
    base_r = gc.apply(red, base).mean()
    base_b = gc.apply(blue, base).mean()
    check("黑白混合只提对应色相", up_r > base_r + 0.05 and abs(up_b - base_b) < 0.01,
          f"红 {base_r:.3f}->{up_r:.3f}，蓝 {base_b:.3f}->{up_b:.3f}")

    # ---- 镜头校正：中性位恒等、畸变动边缘、暗角补偿动四角 ----
    for k in ("lens_distort", "lens_vignette", "lens_fringe"):
        z = gc.apply(img, {**d, k: 0.0})
        check(f"镜头 {k} 中性位恒等", float(np.abs(z - img).max()) < 1e-6)
    H, W = img.shape[:2]
    dz = gc.apply(img, {**d, "lens_distort": 60.0})
    edge = float(np.abs(dz - img)[H // 2, 2].max())
    core = float(np.abs(dz - img)[H // 2, W // 2].max())
    check("畸变改边缘不改中心", edge > 0.05 and core < 0.01,
          f"边缘 {edge:.3f} / 中心 {core:.4f}")
    vg = gc.apply(img, {**d, "lens_vignette": 80.0})
    corn = float((vg - img)[2, 2].mean())
    mid = float((vg - img)[H // 2, W // 2].mean())
    check("暗角补偿提亮四角", corn > 0.02 and abs(mid) < 0.005,
          f"角 {corn:+.3f} / 中心 {mid:+.4f}")

    # ---- 局部蒙版：只影响权重区域，反选换一边 ----
    lin = dict(gc.new_mask("linear"))
    lin.update(x1=0.5, y1=0.25, x2=0.5, y2=0.35, feather=0.0)
    lin["params"] = {"exposure": -1.0}
    p = {**d, "masks": [lin]}
    out = gc.apply(img, p)
    top = float(np.abs(out - img)[:int(H * 0.2)].mean())
    bot = float(np.abs(out - img)[int(H * 0.4):].max())
    check("线性蒙版只压起点一侧", top > 0.05 and bot < 1e-6,
          f"内侧 {top:.3f} / 外侧 {bot:.2e}")
    inv = dict(lin)
    inv["invert"] = 1
    o2 = gc.apply(img, {**d, "masks": [inv]})
    check("反选换到另一侧",
          float(np.abs(o2 - img)[:int(H * 0.2)].max()) < 1e-6
          and float(np.abs(o2 - img)[int(H * 0.6):].mean()) > 0.05)
    rad = dict(gc.new_mask("radial"))
    rad.update(x1=0.5, y1=0.5, x2=0.62, y2=0.5, feather=0.0)
    rad["params"] = {"saturation": 100.0}
    o3 = gc.apply(img, {**d, "masks": [rad]})
    inside = float((o3 - img)[H // 2, W // 2].max())
    outside = float(np.abs(o3 - img)[int(H * 0.9), int(W * 0.05)].max())
    check("径向蒙版只改圈内", inside > 0.02 and outside < 1e-6,
          f"圈内 {inside:.3f} / 圈外 {outside:.2e}")
    idle = dict(gc.new_mask("linear"))
    idle["params"] = {}
    check("空参数蒙版不算改动", not gc.changes({**d, "masks": [idle]}))
    check("有增量蒙版算改动", gc.changes(p))
    f = gc.mask_field(64, 96, lin)
    check("权重场尺寸与范围", f.shape == (64, 96)
          and f.min() >= 0.0 and f.max() <= 1.0)

    # ---- 空间项不进 LUT，颜色项进 ----
    ax = np.linspace(0, 1, 8, dtype=np.float32)
    lat = np.stack([a * np.ones((8, 8, 8), np.float32) for a in
                    (ax.reshape(1, 1, -1), ax.reshape(1, -1, 1),
                     ax.reshape(-1, 1, 1))], -1)
    allp = {**d, "lv_black": 30.0, "lv_gamma": 1.3, "mono": 40.0,
            "masks": [lin], "lens_distort": 50.0, "lens_fringe": 40.0}
    lut = gc.apply(lat, allp, for_lut=True)
    check("LUT 路径忽略蒙版与镜头", np.array_equal(
        lut, gc.apply(lat, {**allp, "masks": [], "lens_distort": 0.0,
                            "lens_fringe": 0.0}, for_lut=True)))
    check("LUT 路径带上色阶与黑白", not np.array_equal(lut, np.clip(lat, 0, 1)))


def test_mask_geometry():
    """角度 / 中点 / 强度：几何与权重场必须和画布把手共用一套坐标。"""
    m = dict(gc.new_mask("linear"))
    m.update(x1=0.5, y1=0.25, x2=0.5, y2=0.35, feather=0.0)
    H, W = 200, 300
    w0 = gc.mask_field(H, W, m)
    check("平直蒙版上侧生效", w0[20, 150] > 0.99 and w0[150, 150] < 0.01,
          f"{w0[20, 150]:.2f}/{w0[150, 150]:.2f}")

    # 正角度 = 屏幕上顺时针：起点从上方转到右侧，生效半边也跟着到右边
    rot = dict(m, angle=90.0)
    w1 = gc.mask_field(H, W, rot)
    check("角度 90 把渐变转到水平", w1[150, 100] < 0.01
          and w1[150, 280] > 0.99,
          f"左 {w1[150, 100]:.2f} / 右 {w1[150, 280]:.2f}")

    mid = dict(m, midpoint=100.0)
    w2 = gc.mask_field(H, W, mid)
    check("中点 100 把过渡线推向终点", w0[65, 150] < 0.5
          and w2[65, 150] > 0.8 and w2[75, 150] < 0.6,
          f"y65 {w0[65, 150]:.2f}->{w2[65, 150]:.2f}")

    half = gc.mask_field(H, W, dict(m, amount=50.0))
    check("强度 50 权重减半", abs(float(half.max()) - 0.5) < 1e-3,
          f"max {half.max():.3f}")
    off = dict(m, amount=0.0)
    off["params"] = {"exposure": -2.0}
    base = np.full((16, 24, 3), 0.5, np.float32)
    d = gc.defaults()
    check("强度 0 完全不生效",
          np.allclose(gc.apply(base, {**d, "masks": [off]}),
                      gc.apply(base, {**d, "masks": []}))
          and not gc.changes({**d, "masks": [off]}))

    r = dict(gc.new_mask("radial"))
    r.update(x1=0.5, y1=0.5, x2=0.62, y2=0.58, feather=0.0)
    wr0 = gc.mask_field(H, W, r)
    wr1 = gc.mask_field(H, W, dict(r, angle=90.0))
    check("角度 90 把椭圆长轴转竖", wr0[121, 149] < 0.01
          and wr1[121, 149] > 0.99 and wr1[99, 179] < 0.01,
          f"上 {wr0[121, 149]:.2f}->{wr1[121, 149]:.2f}")


def test_cache_budget():
    """按字节淘汰、至少留一条、RLock 可重入且并发不炸。"""
    c = gc._BudgetCache(100)
    c.put("a", np.zeros(60, np.uint8))
    c.put("b", np.zeros(60, np.uint8))
    check("按字节预算做 LRU 淘汰", c.get("a") is None and c.get("b") is not None
          and c.total() <= 100, f"total {c.total()}")
    c2 = gc._BudgetCache(10)
    c2.put("big", np.zeros(100, np.uint8))
    check("超预算仍保留正在用的一条", c2.get("big") is not None)
    c2.put("big2", np.zeros(100, np.uint8))
    check("第二条进来后才淘汰旧条目", c2.get("big") is None)
    with c2.lock:
        with c2.lock:
            reentrant = c2.total() == 100
    check("缓存锁可重入", reentrant)

    import threading
    c3 = gc._BudgetCache(1 << 20)
    errors = []

    def hammer(k):
        try:
            for i in range(300):
                c3.put((k, i % 8), np.zeros(4096, np.float32))
                c3.get((k, (i - 1) % 8))
        except Exception as e:                       # noqa: BLE001
            errors.append(repr(e))

    ts = [threading.Thread(target=hammer, args=(k,)) for k in range(4)]
    for t in ts:
        t.start()
    for t in ts:
        t.join()
    check("缓存并发读写无异常", not errors, errors[:1] and errors[0] or "")


def _tone_stats(img, q):
    """按建议参数算一遍（只走逐点链），返回 (过曝比例, 死黑比例, 中间调)。"""
    out = gc.apply(img, {**gc.defaults(), **q}, for_lut=True)
    lum = np.clip(out, 0.0, 1.0) @ gc.LUMA
    n = max(1, lum.size)
    return (float((lum >= 0.995).sum()) / n, float((lum <= 0.005).sum()) / n,
            float(np.median(lum)))


def test_auto(img):
    """自动校正只许往好的方向走：不推高过曝、不推高死黑、中间调更靠近目标。

    这里量的是「真的算一遍之后的直方图两端」，不是提案里的数字：旧写法用
    百分位估算，暗图拉两档曝光把高光顶爆了自己都不知道。
    """
    rng = np.random.default_rng(31)
    mid = np.clip(0.46 + (img - 0.5) * 0.4, 0, 1)
    dark = np.clip(img * 0.25, 0, 1)
    blown = np.clip(img * 1.8, 0, 1)
    night = np.clip(img * 0.06, 0, 1)
    cast = np.full((80, 120, 3), [0.20, 0.21, 0.30], dtype=np.float32)
    # 暗但有一小片已经顶到边缘：拉曝光必然把那 3% 推过界
    near_blow = np.clip(rng.normal(0.20, 0.06, (300, 400, 3)).astype(np.float32),
                        0, 1)
    flat = near_blow.reshape(-1, 3)
    flat[:int(300 * 400 * 0.03)] = 0.99
    # 黑位已经贴死：不许再往下压
    crushed = np.clip(img * 0.30, 0, 1)
    crushed[:60, :60] = 0.0
    # 灰雾：暗端浮起来了，这种才该压黑位
    hazy = np.clip(img * 0.30 + 0.22, 0, 1)

    cases = (("中间调", mid), ("偏暗", dark), ("过曝", blown), ("夜景", night),
             ("蓝偏", cast), ("暗+近爆高光", near_blow))
    for name, x in cases:
        q = gc.auto_params(x)
        base, after = _tone_stats(x, {}), _tone_stats(x, q)
        check(f"自动[{name}]不加深过曝", after[0] <= base[0] + 0.005,
              f"过曝 {base[0] * 100:.2f}% → {after[0] * 100:.2f}%")
        check(f"自动[{name}]不加深死黑", after[1] <= base[1] + 0.005,
              f"死黑 {base[1] * 100:.2f}% → {after[1] * 100:.2f}%")
        check(f"自动[{name}]中间调更靠近目标",
              abs(after[2] - 0.46) <= abs(base[2] - 0.46) + 0.01,
              f"{base[2]:.3f} → {after[2]:.3f}")

    qb = gc.auto_params(blown)
    check("过曝的图不再加曝光", qb["exposure"] <= 0.0 and qb["highlights"] <= 0.0,
          f"曝光 {qb['exposure']:+.2f} EV · 高光 {qb['highlights']:+.0f}")
    qi = gc.auto_params(mid)
    check("已经曝好的图不动曝光", abs(qi["exposure"]) < 0.05,
          f"{qi['exposure']:+.2f} EV")
    qn = gc.auto_params(night)
    check("夜景拉曝光有上限", 0.3 < qn["exposure"] <= 1.5 + 1e-6,
          f"{qn['exposure']:+.2f} EV")
    qk = gc.auto_params(near_blow)
    check("有近爆高光时不敢硬拉曝光", qk["exposure"] < 0.6,
          f"{qk['exposure']:+.2f} EV")
    qc = gc.auto_params(cast)
    check("蓝偏转暖", qc["temperature"] > 20, f"色温 {qc['temperature']:+.0f}")
    check("暗图提亮", gc.auto_params(dark)["exposure"] > 0.3,
          f"{gc.auto_params(dark)['exposure']:+.2f} EV")
    check("贴死的黑位不再往下压", gc.auto_params(crushed)["blacks"] >= 0.0,
          f"黑位 {gc.auto_params(crushed)['blacks']:+.0f}")
    check("灰雾的浮黑位才压下去", gc.auto_params(hazy)["blacks"] < 0.0,
          f"黑位 {gc.auto_params(hazy)['blacks']:+.0f}")
    keys = {"exposure", "contrast", "highlights", "shadows", "whites", "blacks",
            "temperature", "tint"}
    check("自动建议的参数集不变", set(gc.auto_params(img)) == keys)


def test_speed(img):
    """防退化：旧写法单是 HSL 就要 300ms+，这里给 3 倍余量。"""
    p = gc.preset("复古胶片")
    best = min(_one(lambda: gc.apply(img, p)) for _ in range(3))
    check("预设级预览够快", best < 0.4,
          f"{best * 1000:.0f}ms / {img.shape[1]}×{img.shape[0]}")


def _one(fn):
    a = time.perf_counter()
    fn()
    return time.perf_counter() - a


def main():
    rng = np.random.default_rng(9)
    img = np.clip(rng.random((660, 880, 3), dtype=np.float32) * 1.15, 0, 1)
    print(f"对照图 {img.shape[1]}×{img.shape[0]}，1 个 8bit 台阶 = {LSB:.5f}")
    test_hsl(img)
    test_curve(img)
    test_vignette(img)
    test_new_effects(img)
    test_new_tools(img)
    test_mask_geometry()
    test_cache_budget()
    test_basics(img)
    test_lut_path()
    test_auto(img)
    test_speed(img)
    print("\n" + ("全部通过" if not FAILS else f"失败 {len(FAILS)}: {FAILS}"))
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
