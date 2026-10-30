# -*- coding: utf-8 -*-
"""生成 恒晷 · Aevum Studio 的图标：python apps/make_icon.py

主图 1024 用 QPainter 矢量画，不靠外部素材，改配色改几何都在这个文件里；
再用 Pillow 缩出 16/24/32/48/64/128/256 打包成 assets/app.ico。
Windows 取 exe 图标用的是 .ico 里那一串尺寸，只给 256 的话任务栏和列表视图会糊。

图形只有三个元素：一圈开了缺口的镜头环、从圆心穿向缺口的一束光（同时当表针）、中心轴点。
16px 下只剩「环 + 一撇 + 点」，所以宁可粗、少，不放光圈叶那种小细节。
配色抄界面令牌（crates/aevum-ui/src/theme.rs）：ACCENT #0A84FF、TEAL #64D2FF、
TOP 底色取 SURROUND 那一档，任务栏图标和窗口里的高亮才是同一个蓝。
"""
import math
import os
import sys

from PIL import Image
from PySide6 import QtCore, QtGui

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSETS = os.path.join(ROOT, "assets")
SIZE = 1024
TOP = QtGui.QColor("#17181B")
BOTTOM = QtGui.QColor("#0C0C0E")
EDGE = QtGui.QColor(255, 255, 255, 26)
ACCENT = QtGui.QColor("#0A84FF")
TEAL = QtGui.QColor("#64D2FF")
HOT = QtGui.QColor("#DFF1FF")

RING_R = 300.0
# 环的缺口留在右上：光从那儿出去，那位置也正好是 1—2 点的表针。
GAP_CENTER_DEG = -50.0
GAP_DEG = 62.0


def _unit(deg):
    a = math.radians(deg)
    return math.cos(a), math.sin(a)


def _at(cx, cy, dx, dy, dist):
    return QtCore.QPointF(cx + dx * dist, cy + dy * dist)


def draw(path):
    img = QtGui.QImage(SIZE, SIZE, QtGui.QImage.Format.Format_ARGB32_Premultiplied)
    img.fill(QtCore.Qt.GlobalColor.transparent)
    p = QtGui.QPainter(img)
    p.setRenderHint(QtGui.QPainter.RenderHint.Antialiasing)

    panel = QtCore.QRectF(48, 48, SIZE - 96, SIZE - 96)
    clip = QtGui.QPainterPath()
    clip.addRoundedRect(panel, 190, 190)
    p.setClipPath(clip)                       # 晕和环都不许画出圆角面板
    base = QtGui.QLinearGradient(0, 48, 0, SIZE - 48)
    base.setColorAt(0.0, TOP)
    base.setColorAt(1.0, BOTTOM)
    p.fillPath(clip, QtGui.QBrush(base))

    cx = cy = SIZE / 2
    dx, dy = _unit(GAP_CENTER_DEG)
    px, py = -dy, dx

    # 进光处的晕：偏到缺口那一侧，纯居中看着像按钮而不是镜头。
    gx, gy = cx + dx * 96, cy + dy * 96
    glow = QtGui.QRadialGradient(gx, gy, 470)
    glow.setColorAt(0.0, QtGui.QColor(ACCENT.red(), ACCENT.green(), ACCENT.blue(), 74))
    glow.setColorAt(1.0, QtGui.QColor(ACCENT.red(), ACCENT.green(), ACCENT.blue(), 0))
    p.setPen(QtCore.Qt.PenStyle.NoPen)
    p.setBrush(glow)
    p.drawEllipse(QtCore.QPointF(gx, gy), 470, 470)

    # 镜头环：留缺口，从蓝走到青。Qt 的角度逆时针、单位 1/16 度，
    # 而屏幕 y 轴向下，所以角度一律取负。
    grad = QtGui.QLinearGradient(cx - RING_R, cy + RING_R, cx + RING_R, cy - RING_R)
    grad.setColorAt(0.0, ACCENT)
    grad.setColorAt(1.0, TEAL)
    pen = QtGui.QPen(QtGui.QBrush(grad), 34)
    pen.setCapStyle(QtCore.Qt.PenCapStyle.RoundCap)
    p.setPen(pen)
    box = QtCore.QRectF(cx - RING_R, cy - RING_R, 2 * RING_R, 2 * RING_R)
    start = -(GAP_CENTER_DEG + GAP_DEG / 2.0)
    p.drawArc(box, int(start * 16), int((360.0 - GAP_DEG) * 16))

    # 光束：从圆心穿向缺口，越外越宽越亮——既是进光也是表针。
    # 束头必须停在环的内沿（RING_R - 半笔宽）之前，压着环画会像画歪了。
    near, far = 74.0, RING_R - 52.0
    half_near, half_far = 15.0, 30.0
    a = _at(cx, cy, dx, dy, near)
    b = _at(cx, cy, dx, dy, far)
    beam = QtGui.QPainterPath()
    beam.moveTo(a.x() - px * half_near, a.y() - py * half_near)
    beam.lineTo(b.x() - px * half_far, b.y() - py * half_far)
    beam.lineTo(b.x() + px * half_far, b.y() + py * half_far)
    beam.lineTo(a.x() + px * half_near, a.y() + py * half_near)
    beam.closeSubpath()
    beam_grad = QtGui.QLinearGradient(a, b)
    beam_grad.setColorAt(0.0, ACCENT)
    beam_grad.setColorAt(0.55, TEAL)
    beam_grad.setColorAt(1.0, HOT)
    p.setPen(QtCore.Qt.PenStyle.NoPen)
    p.setBrush(beam_grad)
    p.drawPath(beam)

    # 束头收成一个圆帽，免得末端是两个尖角。
    p.setBrush(HOT)
    p.drawEllipse(b, half_far, half_far)

    # 中心轴：光和针都从这一点出发，缺了它就是两块不相干的色。
    p.setBrush(QtGui.QColor(ACCENT.red(), ACCENT.green(), ACCENT.blue(), 130))
    p.drawEllipse(QtCore.QPointF(cx, cy), 56, 56)
    p.setBrush(TOP)
    p.drawEllipse(QtCore.QPointF(cx, cy), 34, 34)
    p.setBrush(HOT)
    p.drawEllipse(QtCore.QPointF(cx, cy), 22, 22)

    p.setPen(QtGui.QPen(EDGE, 6))
    p.setBrush(QtCore.Qt.BrushStyle.NoBrush)
    p.drawRoundedRect(panel, 190, 190)
    p.end()
    img.save(path)


def to_ico(png, ico):
    im = Image.open(png).convert("RGBA")
    im.save(ico, sizes=[(s, s) for s in (256, 128, 64, 48, 32, 24, 16)])


def main():
    os.makedirs(ASSETS, exist_ok=True)
    png = os.path.join(ASSETS, "app.png")
    ico = os.path.join(ASSETS, "app.ico")
    draw(png)
    to_ico(png, ico)
    print(png, os.path.getsize(png) // 1024, "KB")
    print(ico, os.path.getsize(ico) // 1024, "KB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
