# -*- coding: utf-8 -*-
"""去闪烁核心回归：测光、稳健平滑、序列扫描、坏帧与取消。

不启动 Qt：核心层只依赖 numpy/OpenCV，GUI 只负责把它放进后台线程。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_deflicker.py
"""
import math
import os
import shutil
import sys

import cv2
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "acceptance"))
TMP = os.path.join(HERE, "_tmp", "deflicker")
os.makedirs(TMP, exist_ok=True)

from reference import deflicker as flicker    # noqa: E402

FAILS = []


def check(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}"
          + (f"  {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def lin_to_u8(v):
    v = min(max(float(v), 0.0), 1.0)
    s = 12.92 * v if v <= 0.0031308 else 1.055 * v ** (1 / 2.4) - 0.055
    return int(round(s * 255))


def write_frame(path, linear, w=96, h=64):
    cv2.imwrite(path, np.full((h, w, 3), lin_to_u8(linear), np.uint8))


def test_measure():
    im = np.full((40, 60, 3), 128, np.uint8)
    v = flicker.measure_bgr(im)
    check("测光在 sRGB 反解后的线性域", abs(v - 0.21586) < 0.002, f"{v:.5f}")
    check("纯黑帧测得 0", flicker.measure_bgr(
        np.zeros((8, 8, 3), np.uint8)) == 0.0)
    check("空图返回 None", flicker.measure_bgr(None) is None)


def test_offsets():
    n = 12
    gains = np.array([1.0, 1.6, 0.8, 1.4, 0.9, 1.3,
                      0.85, 1.25, 1.0, 1.1, 0.95, 1.0])
    ramp = np.linspace(1.0, 1.2, n)
    means = 0.12 * gains * ramp
    ev, stats = flicker.analyze_means(means, 9, 100)
    check("偏移方向正确（亮帧压暗、暗帧提亮）",
          ev[1] < -0.3 and ev[2] > 0.3,
          f"亮 {ev[1]:+.2f} / 暗 {ev[2]:+.2f}")
    check("相邻帧抖动明显下降",
          stats["jitter_after"] < stats["jitter_before"] * 0.5,
          f"{stats['jitter_before']:.3f} → {stats['jitter_after']:.3f}")
    check("偏移上限不超过 ±3 EV",
          float(np.max(np.abs(ev))) <= flicker.MAX_EV + 1e-9,
          f"{np.max(np.abs(ev)):.2f}")

    slow = 0.1 * 2 ** (np.arange(60) * 0.012)
    sev = flicker.offsets_from_lumas(slow, 15)
    check("真实亮度渐变不被抹平",
          float(np.max(np.abs(sev))) < 0.03,
          f"max {np.max(np.abs(sev)):.4f}")

    flat = np.full(40, 0.2)
    flat[20] = 0.4
    sev = flicker.offsets_from_lumas(flat, 9)
    check("单帧尖峰被识别", -1.15 < sev[20] < -0.75, f"{sev[20]:+.3f}")
    check("尖峰邻居保持稳定",
          max(abs(sev[15]), abs(sev[25])) < 0.15,
          f"{sev[15]:+.3f} / {sev[25]:+.3f}")

    spike = np.full(21, 0.2)
    spike[10] = 0.4
    full = flicker.offsets_from_lumas(spike, 9, 100)[10]
    half = flicker.offsets_from_lumas(spike, 9, 50)[10]
    check("强度 50% 只补一半", abs(half - full / 2) < 0.05,
          f"{full:+.2f} → {half:+.2f}")
    check("两帧序列不产生校正",
          np.allclose(flicker.offsets_from_lumas([0.1, 0.2], 9), 0.0))


def test_channels():
    # 一张纯红图：cv2 存的是 BGR，分通道测光得按 R/G/B 交回来
    red = np.zeros((20, 30, 3), np.uint8)
    red[..., 2] = 255
    rgb = flicker.measure_rgb(red)
    check("分通道按 R/G/B 返回",
          rgb[0] > 0.9 and rgb[1] < 1e-6 and rgb[2] < 1e-6, str(rgb))
    check("空图分通道也返回 None", flicker.measure_rgb(None) is None)

    n = 12
    # 红蓝等量反向、绿通道按亮度权重补回去：整帧亮度一个数都不变，只有颜色在闪
    kk = (flicker.LUMA_RGB[0] - flicker.LUMA_RGB[2]) / flicker.LUMA_RGB[1]
    d = np.where(np.arange(n) % 2, 1.0, -1.0) * 0.05
    means = np.stack([0.20 + d, 0.20 - kk * d, 0.20 - d], axis=1)
    lum = means @ flicker.LUMA_RGB
    lum_ev = flicker.offsets_from_lumas(lum, 9)
    check("亮度不跳，纯亮度去闪无事可做",
          float(np.max(np.abs(lum - 0.20))) < 1e-12
          and float(np.max(np.abs(lum_ev))) < 0.02,
          f"亮度偏差 {np.max(np.abs(lum - 0.20)):.1e} / 偏移 {np.max(np.abs(lum_ev)):.4f}")
    ev, st = flicker.analyze_means(means, 9, 100)
    check("分通道偏移是每帧三个", ev.shape == (n, 3), str(ev.shape))
    check("色闪被认出来（红蓝反向补）",
          ev[1][0] < -0.2 and ev[1][2] > 0.2, f"{np.round(ev[1], 3)}")
    check("绿通道只跟着微调", abs(ev[1][1]) < abs(ev[1][0]) * 0.5,
          f"{ev[1][1]:+.3f} vs {ev[1][0]:+.3f}")
    check("统计里带色闪前后",
          st["channels"] and st["color_before"] > st["color_after"] > 0,
          f"{st['color_before']:.3f} → {st['color_after']:.3f}")
    # 色闪看的是相邻帧的颜色跳变（P95 EV）；整段慢慢飘不是闪
    check("亮度通道的抖动本来就是零", st["jitter_before"] < 1e-3,
          f"{st['jitter_before']:.4f}")
    check("单遍把相邻帧色闪压掉大半",
          st["color_after"] < st["color_before"] * 0.25,
          f"{st['color_before']:.3f} → {st['color_after']:.3f} EV")
    ev2, st2 = flicker.analyze_means(means, 9, 100, multi=True)
    check("多遍把剩下的色闪继续压掉",
          st2["color_after"] < st["color_after"],
          f"{st['color_after']:.3f} → {st2['color_after']:.3f} EV")
    check("补色闪不会把亮度重新顶跳",
          st2["jitter_after"] < 0.02,
          f"亮度抖动 {st2['jitter_before']:.4f} → {st2['jitter_after']:.4f}")

    # 多遍：短窗先挑逐帧闪烁，长窗再把被啃掉的真实渐变添回去
    i = np.arange(48)
    truth = 0.2 * 2 ** (i * 0.02)
    sig = truth * (1 + 0.18 * (-1.0) ** i)
    lg = np.log2(sig)

    def off_truth(off):
        return float(np.abs(lg + off - np.log2(truth)).max())

    def residual(off):
        cor = lg + off
        return float(np.max(np.abs(cor - flicker._gauss(
            flicker._median(cor, 15), 15 / 4.0))))

    one = flicker.offsets_from_lumas(sig, 15)
    two = flicker.offsets_from_lumas(sig, 15, multi=True)
    check("多遍压闪力度不比单遍差", residual(two) <= residual(one) * 1.3,
          f"{residual(one):.4f} → {residual(two):.4f}")
    check("多遍离真值更近（不靠啃渐变换平滑）",
          off_truth(two) < off_truth(one) - 0.02,
          f"{off_truth(one):.3f} → {off_truth(two):.3f} EV")
    check("多遍仍守 ±3 EV",
          float(np.max(np.abs(two))) <= flicker.MAX_EV + 1e-9)
    slow = 0.1 * 2 ** (i * 0.012)
    check("多遍不抹平真实渐变",
          float(np.max(np.abs(flicker.offsets_from_lumas(
              slow, 15, multi=True)))) < 0.05,
          f"{np.max(np.abs(flicker.offsets_from_lumas(slow, 15, multi=True))):.4f}")


def test_scan():
    d = os.path.join(TMP, "seq")
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d, exist_ok=True)
    gains = [1.0, 1.5, 0.85, 1.35, 0.9, 1.2, 1.0, 1.1]
    paths = []
    for i, g in enumerate(gains):
        p = os.path.join(d, f"f_{i:03d}.png")
        write_frame(p, 0.11 * g * (1 + i * 0.01))
        paths.append(p)
    seen = []
    res = flicker.scan(paths, 7, 100,
                       progress=lambda f, t: seen.append(f))
    check("扫描返回每帧偏移", res is not None
          and len(res["offsets"]) == len(paths) and len(res["bad"]) == 0,
          f"{res and len(res['offsets'])} 帧")
    check("扫描进度到 100%", bool(seen) and abs(seen[-1] - 1.0) < 1e-9,
          f"{seen[-1]:.2f}" if seen else "无进度")
    check("扫描的校正方向正确",
          res["offsets"][1] < -0.2 and res["offsets"][2] > 0.2,
          f"{res['offsets'][1]:+.2f} / {res['offsets'][2]:+.2f}")
    check("扫描跳过坏图并继续", (lambda r: r is not None
          and len(r["offsets"]) == 3 and len(r["bad"]) == 1
          and r["offsets"][1] == 0.0)(
              flicker.scan([paths[0], os.path.join(d, "missing.png"),
                            paths[1]], 3, 100)),
          "坏图偏移为 0")
    check("取消立即返回 None",
          flicker.scan(paths, 7, 100, cancel=lambda: True) is None)

    m = flicker.sanitize_means([0.1, "坏", -2, float("nan"), 0.3], 5)
    check("坏测光值被清成无效",
          math.isfinite(m[0]) and all(math.isnan(x) for x in m[1:4])
          and abs(m[4] - 0.3) < 1e-12, str(m))
    res3 = flicker.scan(paths, 7, 100, channels=True, multi=True)
    check("分通道扫描：每帧三个偏移",
          len(res3["offsets"]) == len(paths)
          and all(len(o) == 3 for o in res3["offsets"])
          and len(res3["means"][0]) == 3, str(res3["offsets"][1]))
    m3 = flicker.sanitize_means([[0.1, 0.2, 0.3], ["坏", None, -1]], 2)
    check("分通道旁车逐项清理",
          m3[0] == [0.1, 0.2, 0.3] and all(math.isnan(x) for x in m3[1]),
          str(m3))


def test_apply_chan():
    """端到端：每帧三个 EV 喂进 grade.apply，色闪真的压下去了。

    只验到「算得出偏移」不够，得看补完以后的像素。
    """
    from reference import grade

    d = os.path.join(TMP, "chan")
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d, exist_ok=True)
    frames, paths = [], []
    for i in range(16):
        alt = 1 if i % 2 else -1
        im = np.zeros((32, 24, 3), np.uint8)
        im[...] = (int(120 - alt * 26), 120, int(120 + alt * 26))
        p = os.path.join(d, f"c_{i:03d}.png")
        cv2.imwrite(p, im)
        frames.append(im)
        paths.append(p)
    res = flicker.scan(paths, 9, 100, channels=True, multi=True)

    def jitter(rows):
        a = np.array([flicker.measure_rgb(r) for r in rows], dtype=np.float64)
        lg = np.log2(np.maximum(a, 1e-7))
        # 色闪 = 三个通道里最抖的那个的相邻帧 P95
        return float(max(np.percentile(np.abs(np.diff(lg[:, c])), 95)
                         for c in range(3))), lg.mean(0)

    def u8(rgb):
        return (np.clip(rgb, 0.0, 1.0)[..., ::-1] * 255.0 + 0.5).astype(np.uint8)

    before, lum_before = jitter(frames)
    after_rows = []
    for im, off in zip(frames, res["offsets"]):
        x = im[..., ::-1].astype(np.float32) / 255.0   # BGR -> RGB，sRGB 显示域
        p = dict(grade.DEFAULT)
        p["chan_ev"] = [float(v) for v in off]
        after_rows.append(u8(grade.apply(x, p)))
    after, lum_after = jitter(after_rows)
    check("分通道补完色闪真的变小", after < before * 0.4,
          f"色闪 {before:.2f} → {after:.2f} EV")
    check("补色闪不改变整体明暗",
          float(np.max(np.abs(lum_after - lum_before))) < 0.12,
          f"亮度位移 {np.max(np.abs(lum_after - lum_before)):.3f} EV")
    check("分通道校正不动灰帧",
          np.allclose(flicker.offsets_from_lumas(
              [[0.2, 0.2, 0.2]] * 8, 5, 100, multi=True), 0.0))


def main():
    test_measure()
    test_offsets()
    test_channels()
    test_scan()
    test_apply_chan()
    print("\n" + ("全部通过" if not FAILS else f"失败 {len(FAILS)}: {FAILS}"))
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
