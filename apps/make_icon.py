#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""重新生成 assets/app.png 与 assets/app.ico。

用法（仓库根目录）：`python apps/make_icon.py`，写完直接覆盖两个文件。
要求 Pillow（acceptance 的 .venv 里有）。

图标的构成（2026-11 换版）：
- 字母 **A** = Aevum 的首字母；
- 横杠用品牌蓝 = **晷**——晷针投下的那道影。
  一个字母同时说了「我们叫什么」和「我们是做什么的」。
- 圆角方底、深藏青、全平面色：16px 任务栏尺寸下 A 与蓝杠仍然可辨。

几何全部用多边形手绘（不依赖字体渲染），4 倍超采样后 LANCZOS 缩小，
保证任意尺寸边缘干净。改任何一处坐标，重跑本脚本即可。

---

Regenerate `assets/app.png` and `assets/app.ico`.

Usage (repo root): `python apps/make_icon.py`; both files are overwritten in place.
Requires Pillow (the acceptance venv has it).

What the mark is (the 2026-11 revision):
- The letter **A** stands for Aevum;
- the crossbar is brand blue -- the **shadow** a gnomon casts. One letter says both
  what we are called and what we do.
- Rounded-square background, deep navy, all flat colour: the A and its blue bar stay
  legible down to 16 px taskbar size.

All geometry is hand-drawn polygons (no font rendering), drawn at 4x supersampling and
reduced with LANCZOS so edges stay clean at any size. Change a coordinate, rerun.
"""

from __future__ import annotations

import os

from PIL import Image, ImageDraw

SS = 4  # supersample factor
S = 1024  # nominal canvas
W = S * SS

BG = (11, 19, 33, 255)  # deep navy rounded square
INK = (236, 241, 247, 255)  # off-white strokes
ACC = (59, 158, 255, 255)  # brand blue -- the shadow bar
RADIUS = 225  # background corner radius, nominal px

# A 的几何（名义坐标）：apex = 顶点，feet = 两脚，SW = 笔画宽，BAR = 横杠高度比例。
# Geometry of the A (nominal coordinates).
APEX = (512.0, 296.0)
FEET = (268.0, 772.0), (756.0, 772.0)
SW = 78.0  # stroke width
BAR = 0.66  # crossbar height as a fraction of the apex->feet drop

ICO_SIZES = [16, 24, 32, 48, 64, 128, 256]

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def sc(*pts):
    return [(x * SS, y * SS) for x, y in pts]


def thick_segment(d, a, b, w, color):
    """A segment with flat ends, as a polygon (PIL lines always round their caps)."""
    (x0, y0), (x1, y1) = a, b
    dx, dy = x1 - x0, y1 - y0
    length = (dx * dx + dy * dy) ** 0.5
    ux, uy = -dy / length * w / 2, dx / length * w / 2  # unit perpendicular * w/2
    d.polygon(sc((x0 + ux, y0 + uy), (x1 + ux, y1 + uy), (x1 - ux, y1 - uy), (x0 - ux, y0 - uy)),
              fill=color)


def draw_mark(d):
    """整体轮廓一次成型：外轮廓（尖顶）→ 挖内三角 → 画蓝杠。

    两脚分开画再在顶点拼接，平头端盖总会在交点处露馅（豁口或帽子），
    所以 A 的外轮廓用单个六点多边形：斜接尖顶 + 两脚水平切底。

    The A is drawn as ONE outline polygon. Drawing the legs separately always leaves
    an artefact at the apex (a notch or a hat), so: six-point silhouette with a miter
    apex and flat feet, then punch the counter, then lay the blue bar between the
    inner edges.
    """

    def x_at(y, p0, p1):
        """直线 p0->p1 在给定 y 处的 x。x on the line p0->p1 at a given y."""
        t = (y - p0[1]) / (p1[1] - p0[1])
        return p0[0] + (p1[0] - p0[0]) * t

    fl, fr = FEET
    drop = FEET[0][1] - APEX[1]
    # 底部水平切底时的四只角 / the four bottom corners (flat, horizontal cut)
    lo = (fl[0] - SW / 2, fl[1])  # 左脚外 / left foot, outer
    li = (fl[0] + SW / 2, fl[1])  # 左脚内 / left foot, inner
    ri = (fr[0] - SW / 2, fr[1])  # 右脚内 / right foot, inner
    ro = (fr[0] + SW / 2, fr[1])  # 右脚外 / right foot, outer
    # 内外沿的交点：外沿交出尖顶，内沿交出内三角的顶点。
    # Edge intersections: outer edges meet at the tip, inner edges at the counter apex.
    # 两条腿对称，外/内沿都交在对称轴 x = APEX.x 上。The legs are symmetric, so both
    # intersections sit on the axis through the apex.
    def y_on_line(x, p, q):
        t = (x - p[0]) / (q[0] - p[0])
        return p[1] + (q[1] - p[1]) * t

    top_y = y_on_line(APEX[0], lo, (lo[0] + (APEX[0] - fl[0]), lo[1] + (APEX[1] - fl[1])))
    ctr_y = y_on_line(APEX[0], li, (li[0] + (APEX[0] - fl[0]), li[1] + (APEX[1] - fl[1])))
    top = (APEX[0], top_y)
    ctr = (APEX[0], ctr_y)

    # 1) 外轮廓 / the silhouette
    d.polygon(sc(top, ro, ri, ctr, li, lo), fill=INK)

    # 2) 蓝杠（影）：两端贴着内沿 / the blue bar, spanning between the inner edges
    bar_h = SW * 0.80
    y1 = APEX[1] + drop * BAR
    y2 = y1 + bar_h
    d.polygon(sc((x_at(y1, ctr, li), y1), (x_at(y1, ctr, ri), y1),
                 (x_at(y2, ctr, ri), y2), (x_at(y2, ctr, li), y2)), fill=ACC)

    # 3) 挖掉杠上与杠下的空腔 / punch the counters above and below the bar
    d.polygon(sc(ctr, (x_at(y1, ctr, li), y1), (x_at(y1, ctr, ri), y1)), fill=BG)
    d.polygon(sc((x_at(y2, ctr, li), y2), (x_at(y2, ctr, ri), y2), ri, li), fill=BG)


def render(size: int) -> Image.Image:
    im = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    d.rounded_rectangle([0, 0, W - 1, W - 1], radius=RADIUS * SS, fill=BG)
    draw_mark(d)
    return im.resize((size, size), Image.LANCZOS)


def main():
    png = os.path.join(ROOT, "assets", "app.png")
    ico = os.path.join(ROOT, "assets", "app.ico")

    master = render(S)
    master.save(png)
    master.save(ico, sizes=[(n, n) for n in ICO_SIZES])
    print(f"wrote {png} ({S}x{S})")
    print(f"wrote {ico} (sizes {ICO_SIZES})")


if __name__ == "__main__":
    main()
