# -*- coding: utf-8 -*-
"""序列体检回归：坏帧/黑帧/虚焦/离群/重复/缺口，以及「正常序列不能误报」。

O02 的验收点全在这里：日转夜那种平滑亮度渐变不算离群（局部中位数吃掉了趋势），
单帧被碰、单帧虚焦、单帧曝光飞了必须精确报出来。GUI 那条链在 test_qa_gui 里守。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_qa.py
"""
import os
import shutil
import sys

import cv2
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, ROOT)
TMP = os.path.join(HERE, "_tmp", "qa")

from lapse import luma as luma_mod      # noqa: E402
from lapse import qa                    # noqa: E402

FAILS = []
TOTAL = [0]


def check(name, ok, detail=""):
    TOTAL[0] += 1
    print(f"[{'PASS' if ok else 'FAIL'}] {name}  {detail}", flush=True)
    if not ok:
        FAILS.append(name)


def base_frame(w=200, h=150, seed=3):
    rng = np.random.default_rng(seed)
    low = cv2.resize(rng.random((h // 8, w // 8)).astype(np.float32),
                     (w, h), interpolation=cv2.INTER_CUBIC) * 90
    mid = cv2.GaussianBlur(rng.normal(0, 1, (h, w)).astype(np.float32),
                           (0, 0), 1.1) * 30
    im = np.clip(low + mid + 90, 0, 255).astype(np.uint8)
    for _ in range(25):
        x, y = int(rng.integers(0, w - 30)), int(rng.integers(0, h - 20))
        cv2.rectangle(im, (x, y), (x + int(rng.integers(6, 26)), y + 12),
                      int(rng.integers(0, 256)), -1)
    return cv2.cvtColor(im, cv2.COLOR_GRAY2BGR)


def write_set(d, frames):
    """frames: [(文件名, 图)]；按给定顺序返回路径。图为 None 时写入坏字节。"""
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    paths = []
    for name, im in frames:
        p = os.path.join(d, name)
        if im is None:                       # 坏文件：扩展名对、内容不是图
            with open(p, "wb") as f:
                f.write(b"not an image at all, just junk bytes")
        else:
            cv2.imwrite(p, im, [cv2.IMWRITE_JPEG_QUALITY, 96])
        paths.append(p)
    return paths


def normal(n=30, w=200, h=150):
    """带真实趋势的序列：亮度按 2%(每帧) 渐变（日转夜的缩影）。"""
    im = base_frame(w, h)
    out = []
    for i in range(n):
        g = 2 ** (i * 0.02)
        f = np.clip(im.astype(np.float32) * g, 0, 255).astype(np.uint8)
        out.append(f)
    return out


def test_clean():
    d = os.path.join(TMP, "clean")
    files = write_set(d, [("clip_%04d.jpg" % i, f)
                          for i, f in enumerate(normal())])
    vals, issues = luma_mod.measure_luma(files)
    found = qa.audit(files, vals)
    check("正常序列零误报", not found, str(found[:6]))
    check("正常序列测光无异常", not issues, str(issues[:3]))


def test_bad_frames():
    d = os.path.join(TMP, "dirty")
    frames = normal()
    frames[12] = np.zeros_like(frames[12])                        # 全黑
    frames[20] = cv2.GaussianBlur(frames[20], (0, 0), 8)          # 虚焦
    frames[25] = np.clip(frames[25].astype(np.float32) * 3.0,
                         0, 255).astype(np.uint8)                 # 曝光飞了
    files = write_set(d, [("clip_%04d.jpg" % i, f)
                          for i, f in enumerate(frames)])
    vals, _ = luma_mod.measure_luma(files)
    found = qa.audit(files, vals)
    kinds = {i: k for i, k, _v, _m in found}
    check("精确报出三帧", sorted(kinds) == [12, 20, 25], str(found))
    check("第 12 帧是全黑", kinds.get(12) == "black", str(kinds.get(12)))
    check("第 20 帧是虚焦", kinds.get(20) == "blur", str(kinds.get(20)))
    check("第 25 帧是亮度离群", kinds.get(25) == "luma_out", str(kinds.get(25)))
    check("黑帧不再重复报虚焦/离群",
          all(k != "blur" for i, k, _v, _m in found if i == 12), str(found))
    check("说明写清数值",
          all(m for _i, _k, _v, m in found), "每条都有文字")
    blur_msg = next(m for i, k, _v, m in found if k == "blur")
    check("糊到几百分之一不写成 0%",
          "0%" not in blur_msg and "1/" in blur_msg, blur_msg)
    counts = qa.count_by_kind(found)
    check("汇总按类型计数",
          counts == {"black": 1, "blur": 1, "luma_out": 1}, str(counts))


def test_trend_not_outlier():
    """整段亮度翻倍也不能报成离群：趋势由局部中位数吃掉。"""
    d = os.path.join(TMP, "trend")
    im = base_frame()
    frames = [np.clip(im.astype(np.float32) * (2 ** (i * 0.08)), 0,
                      255).astype(np.uint8) for i in range(30)]
    files = write_set(d, [("clip_%04d.jpg" % i, f) for i, f in enumerate(frames)])
    vals, _ = luma_mod.measure_luma(files)
    found = [r for r in qa.audit(files, vals) if r[1] == "luma_out"]
    check("每帧 +8% 的陡渐变不误报离群", not found, str(found[:4]))


def test_step_and_short():
    """曝光台阶（Holy Grail）与太短的序列都不能报离群。"""
    d = os.path.join(TMP, "step")
    im = base_frame()
    frames = [np.clip(im.astype(np.float32) * (1.0 if i < 15 else 1.6), 0,
                      255).astype(np.uint8) for i in range(30)]
    files = write_set(d, [("clip_%04d.jpg" % i, f) for i, f in enumerate(frames)])
    vals, _ = luma_mod.measure_luma(files)
    found = qa.audit(files, vals)
    check("亮度台阶不算离群", not [r for r in found if r[1] in
                                   ("luma_out", "blur")], str(found[:4]))

    d3 = os.path.join(TMP, "short")
    files3 = write_set(d3, [("clip_%04d.jpg" % i, f)
                            for i, f in enumerate(normal(6))])
    vals3, _ = luma_mod.measure_luma(files3)
    check("6 帧短序列不做统计判定",
          not [r for r in qa.audit(files3, vals3)
               if r[1] in ("luma_out", "blur")], str(qa.audit(files3, vals3)))


def test_read_dup_gap():
    d = os.path.join(TMP, "mixed")
    frames = normal(20)
    names = ["clip_%04d.jpg" % i for i in range(20)]
    files = write_set(d, list(zip(names, frames)))
    # 坏文件
    bad = os.path.join(d, "clip_0009.jpg")
    with open(bad, "wb") as f:
        f.write(b"junkjunkjunk")
    # 重复帧：把 13 号原样再存一份当 14 号
    shutil.copyfile(files[13], files[14])
    vals, _ = luma_mod.measure_luma(files)
    found = qa.audit(files, vals, dup=True)
    kinds = {i: k for i, k, _v, _m in found}
    check("坏文件报读不出", kinds.get(9) == "read", str(kinds.get(9)))
    check("重复帧报 dup", kinds.get(14) == "dup", str(kinds.get(14)))
    check("读不出的帧不叠加离群",
          all(k not in ("blur", "luma_out") for i, k in kinds.items()
              if i == 9), str(found))

    d2 = os.path.join(TMP, "gap")
    shutil.rmtree(d2, ignore_errors=True)
    os.makedirs(d2)
    files2 = []
    for i, f in enumerate(frames):
        if i in (5, 6, 7):
            continue
        p = os.path.join(d2, "clip_%04d.jpg" % i)
        cv2.imwrite(p, f, [cv2.IMWRITE_JPEG_QUALITY, 96])
        files2.append(p)
    vals2, _ = luma_mod.measure_luma(files2)
    g = [r for r in qa.audit(files2, vals2) if r[1] == "gap"]
    check("缺口被报出", len(g) == 1 and g[0][2] == 3, str(g))
    check("缺口指向后一帧", g and g[0][0] == 5, str(g))
    check("缺口不可排除", g and not qa.excludable(g[0][1]))
    check("关闭 gaps 后只剩亮度结论",
          not [r for r in qa.audit(files2, vals2, gaps=False)
               if r[1] == "gap"])


def main():
    os.makedirs(TMP, exist_ok=True)
    test_clean()
    test_bad_frames()
    test_trend_not_outlier()
    test_step_and_short()
    test_read_dup_gap()
    print(f"\n{TOTAL[0] - len(FAILS)}/{TOTAL[0]} 通过"
          + (f"  失败: {FAILS}" if FAILS else ""))
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
