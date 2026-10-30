# -*- coding: utf-8 -*-
"""裁剪：选区几何单测 + 三种模式的端到端出片校验。

选区几何是纯函数（lapse/crop.py），先单独钉死；再走一遍真界面：
装载视频 -> 自动居中选区 -> 比特流无损出片（码流没重编码，画面逐像素不变）
-> 重编码出片（分辨率真的变小）-> 取消不留半成品。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_crop.py
"""
import os
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, ROOT)
TMP = os.path.join(HERE, "_tmp")
SHOTS = os.path.join(ROOT, "shots")
os.makedirs(TMP, exist_ok=True)

import cv2                                    # noqa: E402
import numpy as np                            # noqa: E402
from PySide6 import QtCore, QtWidgets         # noqa: E402

from lapse import crop as cp                  # noqa: E402
from lapse.ff import find_ffmpeg, probe_video, run as ff_run   # noqa: E402
from lapse.gui import cropper, theme, workbench                # noqa: E402

FF = find_ffmpeg()
SRC = os.path.join(TMP, "_crop_src.mp4")
FAILS = []


def check(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}"
          + (f"  {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def pump(app, ms):
    t0 = time.time()
    while (time.time() - t0) * 1000 < ms:
        app.processEvents()
        QtCore.QThread.msleep(5)
    app.processEvents()


def even(v):
    return v % 2 == 0


def ratio(sel, a):
    """选区是否严格等于给定比例（用交叉相乘，避开浮点）。"""
    return a and sel[2] * a[1] == sel[3] * a[0]


def inside(sel, W, H):
    x, y, w, h = sel
    return x >= 0 and y >= 0 and w >= 2 and h >= 2 and x + w <= W and y + h <= H


def test_geometry():
    W, H = 640, 360
    check("选区取偶", cp.norm_rect(1, 1, 101, 51, W, H) == (0, 0, 100, 50),
          str(cp.norm_rect(1, 1, 101, 51, W, H)))
    check("选区反手也能画", cp.norm_rect(300, 200, 100, 50, W, H) == (100, 50, 200, 150))
    check("选区不出画面", inside(cp.norm_rect(600, 340, 900, 900, W, H), W, H),
          str(cp.norm_rect(600, 340, 900, 900, W, H)))
    check("居中 16:9", cp.center_rect(640, 400, (16, 9)) == (0, 20, 640, 360),
          str(cp.center_rect(640, 400, (16, 9))))
    check("自由=整幅取偶", cp.center_rect(641, 361, None) == (0, 0, 640, 360),
          str(cp.center_rect(641, 361, None)))
    r = cp.refit_rect((0, 0, 640, 360), W, H, (1, 1))
    check("换比例绕中心", r == (140, 0, 360, 360) and ratio(r, (1, 1)), str(r))

    a = (16, 9)
    for pt in ((300, 250), (0, 0), (-200, -200), (900, 900), (639, 359)):
        s = cp.draw_rect((100, 100), *pt, W, H, a)
        if not (inside(s, W, H) and ratio(s, a) and even(s[2]) and even(s[3])):
            check("拖框锁比例", False, f"{pt} -> {s}")
            break
    else:
        check("拖框锁比例", True)

    bad = []
    for handle in ("nw", "n", "ne", "e", "se", "s", "sw", "w"):
        for pt in ((0, 0), (639, 359), (-500, -500), (900, 900), (320, 180)):
            for aa in (a, (1, 1), (9, 16), None):
                s = cp.resize_rect(handle, (100, 100, 200, 112), *pt, W, H, aa)
                if not inside(s, W, H) or (aa and not ratio(s, aa)):
                    bad.append(f"{handle}{pt}{aa}->{s}")
    check("八个把手都锁比例且不越界", not bad, "; ".join(bad[:3]))

    n = cp.nudge_rect((638, 358, 2, 2), 1, 1, 2, W, H)
    check("顶到边不停在奇数", n == (638, 358, 2, 2), str(n))
    n2 = cp.nudge_rect((100, 100, 200, 112), -1, 1, 20, W, H)
    check("Shift 加速", n2 == (80, 120, 200, 112), str(n2))

    rect = (10, 10, 100, 80)
    check("命中检测分角/边/内/外",
          (cp.hit_test(rect, 10, 10), cp.hit_test(rect, 55, 10),
           cp.hit_test(rect, 55, 45), cp.hit_test(rect, 3, 45))
          == ("corner:nw", "edge:n", "in", "out"),
          str([cp.hit_test(rect, *p) for p in
               ((10, 10), (55, 10), (55, 45), (3, 45))]))


def test_cmds():
    info = {"codec": "h264", "w": 3840, "h": 2160, "conf": (0, 0, 0, 8)}
    cmd = " ".join(cp.build_copy_cmd("ff", "src.mp4", "dst.mp4", info,
                                     (10, 20, 3820, 2120)))
    # 源本身带 8 像素下边裁切：新选区是在显示画面上量的，偏移必须叠加
    check("比特流命令叠加已有裁切",
          "crop_left=10:crop_right=10:crop_top=20:crop_bottom=28" in cmd, cmd)
    cmd2 = " ".join(cp.build_encode_cmd("ff", "src.mp4", "dst.mp4",
                                        (10, 20, 3820, 2120), False, 14, "fast"))
    check("重编码命令带 crop 滤镜与 CRF",
          "-vf crop=3820:2120:10:20" in cmd2 and "-crf 14" in cmd2, cmd2)
    cmd3 = " ".join(cp.build_encode_cmd("ff", "s", "d", (0, 0, 100, 100),
                                        True, None, "medium"))
    check("数学无损带 lossless 参数", "lossless=1" in cmd3 and "-crf" not in cmd3)


def make_src(dur=6):
    if FF is None:
        return False
    r = ff_run([FF, "-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi",
                "-i", f"testsrc2=size=1280x720:rate=25:duration={dur}",
                "-c:v", "libx264", "-preset", "ultrafast",
                "-pix_fmt", "yuv420p", SRC])
    return r.returncode == 0 and os.path.exists(SRC)


def frame(path, t):
    out = os.path.join(TMP, "_crop_f.png")
    if os.path.exists(out):
        os.remove(out)
    ff_run([FF, "-y", "-hide_banner", "-loglevel", "error", "-ss", f"{t}",
            "-i", path, "-frames:v", "1", out])
    return cv2.imread(out)


def wait_job(app, wb, timeout=180):
    t0 = time.time()
    while wb._job is not None and wb._job.isRunning() and time.time() - t0 < timeout:
        pump(app, 60)
    pump(app, 250)
    return wb._job is None or not wb._job.isRunning()


def test_gui(app):
    wb = workbench.Workbench([cropper.Cropper])
    wb.resize(1480, 880)
    wb.show()
    pump(app, 200)
    c = wb.module(cropper.Cropper.name)

    check("模块已注册", list(wb.mod_btns) == [cropper.Cropper.name])
    check("裁剪无停靠面板",
          not [d for d in wb.findChildren(QtWidgets.QDockWidget) if d.isVisible()])
    check("状态栏用模块提示", wb.lb_status.text() == c.hint(), wb.lb_status.text())

    c.load(SRC)
    pump(app, 500)
    check("视频已读取", c.info is not None and c.info["w"] == 1280,
          c.lb_info.text())
    check("预览帧已出图", c.view.pixmap is not None and not c.view.pixmap.isNull())
    sel = c.view.sel
    check("默认按 16:9 居中", sel == (0, 0, 1280, 720), str(sel))
    check("选区同步到数值框",
          (c.sp["x"].value(), c.sp["w"].value()) == (sel[0], sel[2]))
    check("比特流无损可用", c.rb["copy"].isEnabled() and c._mode() == "copy")
    check("copy 模式下 CRF 灰掉", not c.sl_crf.isEnabled()
          and not c.cb_preset.isEnabled())

    # 换比例：选区绕中心重算，严格锁比
    c.cb_aspect.setCurrentText("1:1")
    pump(app, 120)
    s2 = c.view.sel
    check("切 1:1 后严格正方且居中",
          s2[2] == s2[3] and s2[1] == 0 and s2[2] == 720, str(s2))

    # 数值框直接给一个 4:3 偏左上的选区
    c.cb_aspect.setCurrentText("自由")
    for k, v in (("x", 100), ("y", 60), ("w", 800), ("h", 500)):
        c.sp[k].setValue(v)
    c._field_apply()
    pump(app, 100)
    check("数值框生效", c.view.sel == (100, 60, 800, 500), str(c.view.sel))

    # 方向键微调：一格 2 像素，Shift 一格 20
    c.view.keyPressEvent(_key(QtCore.Qt.Key.Key_Right))
    c.view.keyPressEvent(_key(QtCore.Qt.Key.Key_Down, shift=True))
    check("方向键微调", c.view.sel == (102, 80, 800, 500), str(c.view.sel))

    path, size = _shoot(wb, "裁剪")
    # 预览帧是 testsrc2 的纯色块，PNG 压得特别狠，所以这里只要求"不是黑屏"
    check("裁剪界面截图", size > 100000, f"{size // 1024}KB")

    # ---- 模式 1：比特流无损 ----
    out1 = os.path.join(TMP, "_crop_copy.mp4")
    _run_to(c, wb, app, out1, "copy")
    check("比特流无损出片", os.path.exists(out1), c.log_view.toPlainText()[-160:])
    i1 = probe_video(FF, out1) if os.path.exists(out1) else None
    x, y, w, h = c.view.sel
    check("编码尺寸没动（没重编码）",
          i1 and i1["coded"] == c.info["coded"], str(i1["coded"]) if i1 else "")
    check("裁切窗口写进码流了",
          i1 and i1["conf"] == (x, 1280 - x - w, y, 720 - y - h),
          str(i1["conf"]) if i1 else "")
    check("显示尺寸等于选区", i1 and (i1["w"], i1["h"]) == (w, h),
          f"{i1['w']}x{i1['h']}" if i1 else "")
    check("时长与帧率不变", i1 and abs(i1["dur"] - c.info["dur"]) < 0.05
          and abs(i1["fps"] - c.info["fps"]) < 0.01,
          f"{i1['dur']:.2f}s {i1['fps']}fps" if i1 else "")
    a, b = frame(SRC, 2.0), frame(out1, 2.0)
    keep = a[y:y + h, x:x + w] if a is not None and b is not None else None
    check("画面逐像素等于源图选区",
          keep is not None and keep.shape == b.shape
          and float(np.abs(keep.astype(int) - b.astype(int)).mean()) < 0.5,
          "" if keep is None or b is None else
          f"形状 {keep.shape}/{b.shape} 平均差 "
          f"{np.abs(keep.astype(int) - b.astype(int)).mean():.3f}")

    # ---- 模式 2：重编码 ----
    out2 = os.path.join(TMP, "_crop_crf.mp4")
    _run_to(c, wb, app, out2, "crf")
    i2 = probe_video(FF, out2) if os.path.exists(out2) else None
    check("重编码出片", os.path.exists(out2) and i2 is not None)
    # 重编码后 x265 会把编码面对齐到 8/16，多出来的下边缘用裁切窗口遮掉：
    # 显示尺寸必须正好等于选区，coded 比它大是正常的，不能拿 coded 断言
    check("重编码后分辨率真的变小",
          i2 and (i2["w"], i2["h"]) == (w, h) and i2["crop_ok"]
          and i2["coded"][0] >= w and i2["coded"][1] >= h,
          f"{i2['w']}x{i2['h']} coded {i2['coded']} conf {i2['conf']}" if i2 else "")
    check("重编码走 x265", i2 and i2["codec"] in ("hevc", "h265"),
          i2["codec"] if i2 else "")
    check("成片自动进调色台流水线",
          wb.material.kind == "video"
          and os.path.normpath(wb.material.path) == os.path.normpath(out2),
          wb.material.label())
    check("自己的成片不回灌裁剪页",
          os.path.normpath(c.ed_path.text()) == os.path.normpath(SRC),
          c.ed_path.text())

    # ---- 取消不留半成品 ----
    part = os.path.join(TMP, "_crop_cancel.mp4")
    c.out_manual = True
    c.ed_out.setText(part)
    c.run()
    pump(app, 250)
    running = wb._job is not None and wb._job.isRunning()
    wb.cancel_job()
    ok = wait_job(app, wb)
    check("取消前确实在跑", running and wb.prog.value() >= 0)
    check("取消后任务已停", ok)
    check("取消后不留半成品", not os.path.exists(part),
          c.log_view.toPlainText()[-120:].replace("\n", " "))

    # ---- 图片裁剪：一样框选，但直接切像素另存副本 ----
    still = os.path.join(TMP, "_crop_still.png")
    h, w = 420, 640
    yy, xx = np.indices((h, w))
    pic = np.dstack((yy * 255 // h, xx * 255 // w,
                     (xx + yy) * 255 // (w + h))).astype(np.uint8)
    cv2.circle(pic, (470, 210), 90, (255, 255, 255), -1)
    cv2.imwrite(still, pic)
    with open(still, "rb") as f:
        src_bytes = f.read()

    # 走真实的导入路径：顶栏项目卡也要跟着变成图片项目
    wb.open_path(still)
    pump(app, 400)
    check("图片已读取", c.is_image and c.info["w"] == w and c.info["h"] == h,
          c.lb_info.text())
    check("图片成为当前项目", wb.project_kind == "image"
          and "图片" in wb.lb_material.text(), wb.lb_material.text())
    check("图片没有视频模式选项", not c.box_mode.isVisible()
          and c.btn_run.text() == "裁剪并另存", c.btn_run.text())
    c.cb_aspect.setCurrentText("自由")
    for k, v in (("x", 37), ("y", 53), ("w", 400), ("h", 300)):
        c.sp[k].setValue(v)
    c._field_apply()
    pump(app, 100)
    out3 = os.path.join(TMP, "_crop_still_out.png")
    if os.path.exists(out3):
        os.remove(out3)
    c.out_manual = True
    c.ed_out.setText(out3)
    sel = c.view.sel
    check("图片选区同样取偶", sel == (36, 52, 400, 300), str(sel))
    c.run()
    pump(app, 300)
    got = cv2.imread(out3) if os.path.exists(out3) else None
    x, y, w, h = sel
    check("图片按选区切像素", got is not None and got.shape[:2] == (h, w)
          and np.array_equal(got, pic[y:y + h, x:x + w]),
          str(got.shape) if got is not None else "未写出")
    check("原图一字未动", open(still, "rb").read() == src_bytes)
    check("图片成片进调色台流水线",
          wb.material.kind == "image"
          and os.path.normpath(wb.material.path) == os.path.normpath(out3),
          wb.material.label())
    check("图片日志写明尺寸", "图片:" in c.log_view.toPlainText(),
          c.log_view.toPlainText()[-120:].replace("\n", " "))
    path, size = _shoot(wb, "裁剪图片")
    check("图片裁剪界面截图", size > 50000, f"{size // 1024}KB")


def _key(key, shift=False):
    mod = QtCore.Qt.KeyboardModifier.ShiftModifier if shift else \
        QtCore.Qt.KeyboardModifier.NoModifier
    return QtGui_keyevent(key, mod)


def QtGui_keyevent(key, mod):
    from PySide6 import QtGui
    return QtGui.QKeyEvent(QtCore.QEvent.Type.KeyPress, key, mod)


def _shoot(wb, name):
    p = os.path.join(SHOTS, f"shot_{name}.png")
    wb.grab().save(p)
    return p, os.path.getsize(p)


def _run_to(c, wb, app, dst, mode):
    if os.path.exists(dst):
        os.remove(dst)
    c.rb[mode].setChecked(True)
    c.out_manual = True
    c.ed_out.setText(dst)
    c.run()
    assert wait_job(app, wb), f"{mode} 模式任务没在时限内结束"


def main():
    if FF is None or not make_src():
        print("SKIP  没有可用的 ffmpeg，跳过裁剪端到端测试")
        return
    test_geometry()
    test_cmds()
    # 裁剪模块会读上次的模式/比例；测试落到临时 INI，不碰用户偏好
    ini = os.path.join(TMP, "settings")
    shutil.rmtree(ini, ignore_errors=True)
    QtCore.QSettings.setDefaultFormat(QtCore.QSettings.Format.IniFormat)
    QtCore.QSettings.setPath(QtCore.QSettings.Format.IniFormat,
                             QtCore.QSettings.Scope.UserScope, ini)
    app = QtWidgets.QApplication(sys.argv)
    theme.apply(app)
    test_gui(app)
    print("\n全部通过" if not FAILS else f"\n失败 {len(FAILS)} 项：{'、'.join(FAILS)}")
    sys.exit(1 if FAILS else 0)


if __name__ == "__main__":
    main()
