# -*- coding: utf-8 -*-
"""生成 Rust 单测 dehaze_downsample_matches_python 的参考统计量。

与 crates/lumen-core/src/develop/dehaze.rs::tests::hazy_scene 逐式相同：
LCG(6364136223846793005, 1442695040888963407) 每像素抽一次透射，场景
（阶梯 + 渐变）与大气光按 I = J·t + A·(1−t) 在线性域混合。

用法：venv python oracle/dehaze_downsample_ref.py
"""
import math
import sys
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from lapse import grade  # noqa: E402

MASK = (1 << 64) - 1


class LCG:
    def __init__(self, seed):
        self.s = seed & MASK

    def next(self):
        self.s = (self.s * 6364136223846793005 + 1442695040888963407) & MASK
        return np.float32(self.s >> 33) / np.float32(1 << 31)


def srgb_to_lin(v):
    v = np.asarray(v, np.float32)
    return np.where(v <= np.float32(0.04045), v / np.float32(12.92),
                    ((v + np.float32(0.055)) / np.float32(1.055)) ** np.float32(2.4))


def lin_to_srgb(v):
    v = np.asarray(v, np.float32)
    return np.where(v <= np.float32(0.0031308), v * np.float32(12.92),
                    np.float32(1.055) * v ** np.float32(1.0 / 2.4) - np.float32(0.055))


def hazy_scene(w, h, airlight):
    rng = LCG(0xC0FFEE1234567890)
    rgb = np.empty((h, w, 3), np.float32)
    for y in range(h):
        for x in range(w):
            u = np.float32(x) / np.float32(w - 1)
            v = np.float32(y) / np.float32(h - 1)
            scene = (np.float32(0.15) + np.float32(0.6) * np.float32(u > 0.4)
                     + np.float32(0.1) * v,
                     np.float32(0.20) + np.float32(0.5) * v,
                     np.float32(0.25) + np.float32(0.4) * np.float32(u > 0.7)
                     + np.float32(0.1) * (np.float32(1.0) - v))
            t = (np.float32(0.25) + np.float32(0.6) * v
                 + np.float32(0.05) * (rng.next() - np.float32(0.5)))
            for c in range(3):
                lin_j = srgb_to_lin(np.float32(min(max(scene[c], 0.0), 1.0)))
                lin_i = lin_j * t + np.float32(airlight[c]) * (np.float32(1.0) - t)
                rgb[y, x, c] = lin_to_srgb(np.float32(min(max(float(lin_i), 0.0), 1.0)))
    return rgb


def main():
    w, h = 128, 96
    src = hazy_scene(w, h, (0.75, 0.78, 0.82))
    out = grade._dehaze_dcp(src, 0.6, 0.2)
    flat = out.reshape(-1, 3).astype(np.float64)
    means = [float(flat[:, c].mean()) for c in range(3)]
    stds = [float(flat[:, c].std()) for c in range(3)]
    print("mean = [%r, %r, %r]" % tuple(means))
    print("std  = [%r, %r, %r]" % tuple(stds))


if __name__ == "__main__":
    main()
