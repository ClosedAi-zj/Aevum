# -*- coding: utf-8 -*-
"""对齐模块回归：预检不落盘、全量出片、流水线、取消清理、参数记忆。

算法本身的精度归 tests/test_align.py；这里守的是界面接线：
位移合理 -> 写出 aligned_* -> 自动交给合成 -> 取消不留半成品。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_aligner.py
"""
import os
import shutil
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "acceptance"))
TMP = os.path.join(HERE, "_tmp")
os.makedirs(TMP, exist_ok=True)

import cv2                                # noqa: E402
import numpy as np                        # noqa: E402
from PySide6 import QtCore, QtWidgets     # noqa: E402

from reference import align                   # noqa: E402
from reference.gui import assemble, theme, workbench     # noqa: E402
from reference.gui.aligner import Aligner     # noqa: E402

# 入口真正注册的那份模块表：对齐必须和另外三个工具在同一个程序里
from apps.workbench import MODULES        # noqa: E402

SRC = os.path.join(TMP, "al_test_src")
OUT = SRC + "_aligned"
CANCEL_SRC = os.path.join(TMP, "al_cancel_src")
CANCEL_OUT = os.path.join(TMP, "al_cancel_out")
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


def wait_job(app, wb, timeout=120):
    t0 = time.time()
    while wb._job is not None and wb._job.isRunning() and time.time() - t0 < timeout:
        pump(app, 50)
    pump(app, 250)
    return wb._job is None or not wb._job.isRunning()


def make_base(w, h, seed=11):
    rng = np.random.default_rng(seed)
    low = cv2.resize(rng.random((h // 24, w // 24)).astype(np.float32),
                     (w, h), interpolation=cv2.INTER_CUBIC) * 120
    mid = cv2.GaussianBlur(rng.normal(0, 1, (h, w)).astype(np.float32),
                           (0, 0), 2.2) * 26
    im = np.clip(low + mid + 60, 0, 255).astype(np.uint8)
    for _ in range(40):
        x, y = int(rng.integers(0, w - 60)), int(rng.integers(int(h * .68), h - 30))
        cv2.rectangle(im, (x, y), (x + int(rng.integers(10, 60)), y + 20),
                      int(rng.integers(0, 256)), -1)
    return cv2.cvtColor(im, cv2.COLOR_GRAY2BGR)


def make_series(d, n, w=720, h=540):
    """亚像素渐变漂移：相邻帧差 < 1px，median 平滑不会把真位移当噪声。"""
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    base = make_base(w, h)
    for i in range(n):
        f = cv2.warpAffine(
            base, np.float32([[1, 0, i * 0.6], [0, 1, -i * 0.4]]), (w, h),
            borderMode=cv2.BORDER_REPLICATE)
        cv2.imwrite(os.path.join(d, f"al_{i:04d}.jpg"), f,
                    [cv2.IMWRITE_JPEG_QUALITY, 95])
    return d


def test_registration():
    check("对齐已注册进同一个工作台",
          Aligner in MODULES
          and [m.name for m in MODULES]
          == ["调色台", "对齐", "合成", "裁剪", "配乐", "运动"],
          str([m.name for m in MODULES]))


def test_flow(app):
    alg = None
    wb = workbench.Workbench([Aligner, assemble.Assemble])
    wb.resize(1280, 800)
    wb.show()
    pump(app, 200)
    alg = wb.module(Aligner.name)
    asm = wb.module(assemble.Assemble.name)

    shutil.rmtree(OUT, ignore_errors=True)
    alg.ed_dir.setText(SRC)
    pump(app, 900)                       # 目录防抖 500ms
    check("检测到序列", bool(alg.files) and len(alg.files) == 8,
          alg.lb_info.text())
    check("默认输出目录", os.path.normpath(alg.ed_out.text()) ==
          os.path.normpath(OUT), alg.ed_out.text())

    alg.cb_est.setCurrentText("ECC 平移+旋转（相机被碰歪）")
    o = alg._prepare()
    check("ECC 估计方式接到参数", o["est"] == "ecc" and o["motion"] == "euclid",
          f"{o['est']}/{o['motion']}")
    alg.cb_est.setCurrentText("模板匹配（对垂直漂移最稳）")

    # 预检：只测位移，不写任何文件
    alg.probe()
    check("预检跑完", wait_job(app, wb), wb.lb_status.text())
    check("预检不落盘", not os.path.exists(OUT), OUT)
    log = alg.log_view.toPlainText()
    check("预检给出裁切预估", "按采样粗算" in log and "面积损失" in log,
          log[-160:].replace("\n", " "))
    check("预检后按钮复位", alg.btn_probe.isEnabled() and alg.btn_run.isEnabled())

    # 全量：出片 + 校验 + 交给合成
    alg.run()
    check("全量跑完", wait_job(app, wb), wb.lb_status.text())
    outs = sorted(f for f in os.listdir(OUT)) if os.path.isdir(OUT) else []
    check("写出 8 张对齐序列", len(outs) == 8 and outs[0] == "aligned_0000.jpg",
          f"{len(outs)} 张")
    paths = [os.path.join(OUT, f) for f in outs]
    v = align.verify_residuals(paths, ref_index=0)
    mx = max(float(np.hypot(*s)) for _i, s, _r in v if s is not None)
    check("对齐后残余 < 1px", mx < 1.0, f"最大 {mx:.2f}px")
    log = alg.log_view.toPlainText()
    check("日志报告残余", "最大残余" in log, log[-120:].replace("\n", " "))

    check("流水线：素材变成对齐序列",
          wb.material.kind == "series" and os.path.normpath(wb.material.path)
          == os.path.normpath(OUT), wb.material.label())
    check("自己的输出不回灌输入框",
          os.path.normpath(alg.ed_dir.text()) == os.path.normpath(SRC),
          alg.ed_dir.text())
    # 合成收到目录后还有 500ms 防抖才扫描；等 series 出现，别只等输入框
    t0 = time.time()
    while (asm.series is None or asm.series["count"] != 8) \
            and time.time() - t0 < 10:
        pump(app, 50)
    check("流水线：合成模块拿到对齐结果",
          os.path.normpath(asm.ed_dir.text()) == os.path.normpath(OUT)
          and asm.series is not None and asm.series["count"] == 8,
          asm.lb_info.text())
    check("完成后可打开输出", alg.btn_open.isEnabled())

    # 参数记忆：关窗写盘，重开还原（测试用临时 INI，不碰真实偏好）
    alg.sp_smooth.setValue(5)
    alg.sp_jump.setValue(0.7)
    alg.sp_score.setValue(0.65)
    alg.cb_out.setCurrentText("PNG 16bit（像素无损）")
    wb.close()
    pump(app, 200)

    wb2 = workbench.Workbench([Aligner, assemble.Assemble])
    pump(app, 600)                       # _restore 会触发一次目录检测
    alg2 = wb2.module(Aligner.name)
    check("记住上次参数",
          alg2.sp_smooth.value() == 5 and abs(alg2.sp_jump.value() - 0.7) < 1e-6
          and abs(alg2.sp_score.value() - 0.65) < 1e-6
          and alg2.cb_out.currentText() == "PNG 16bit（像素无损）",
          f"{alg2.sp_smooth.value()} {alg2.sp_jump.value()} "
          f"{alg2.sp_score.value()} {alg2.cb_out.currentText()}")
    check("记住上次目录", os.path.normpath(alg2.ed_dir.text())
          == os.path.normpath(SRC), alg2.ed_dir.text())
    wb2.close()
    pump(app, 200)


def test_cancel(app):
    shutil.rmtree(CANCEL_OUT, ignore_errors=True)
    make_series(CANCEL_SRC, 30, w=1000, h=750)
    wb = workbench.Workbench([Aligner])
    wb.resize(1200, 760)
    wb.show()
    pump(app, 200)
    alg = wb.module(Aligner.name)
    alg.ed_dir.setText(CANCEL_SRC)
    pump(app, 900)
    check("取消用例序列就位", bool(alg.files) and len(alg.files) == 30,
          alg.lb_info.text())
    alg.ed_out.setText(CANCEL_OUT)
    # 单 worker 把流程拖慢到可以稳定点到取消；真实用户机器上多核会更快
    orig = alg._opts
    alg._opts = lambda: dict(orig(), workers=1)
    alg.run()
    t0 = time.time()
    while wb.prog.value() == 0 and time.time() - t0 < 60:
        pump(app, 50)
    check("取消前任务在跑", wb._job is not None and wb._job.isRunning())
    wb.cancel_job()
    check("取消跑完", wait_job(app, wb, 60))
    check("取消不留输出", not os.path.isdir(CANCEL_OUT)
          or not os.listdir(CANCEL_OUT), CANCEL_OUT)
    check("取消后有说明", "已取消" in alg.log_view.toPlainText(),
          alg.log_view.toPlainText()[-120:].replace("\n", " "))
    check("取消后按钮复位", alg.btn_run.isEnabled() and alg.btn_probe.isEnabled())
    alg._opts = orig
    wb.close()
    pump(app, 200)


def main():
    # 测试别读写用户真实偏好：换成 tests/_tmp 下的 ini
    ini = os.path.join(TMP, "al_settings")
    shutil.rmtree(ini, ignore_errors=True)
    QtCore.QSettings.setDefaultFormat(QtCore.QSettings.Format.IniFormat)
    QtCore.QSettings.setPath(QtCore.QSettings.Format.IniFormat,
                             QtCore.QSettings.Scope.UserScope, ini)
    make_series(SRC, 8)
    app = QtWidgets.QApplication(sys.argv)
    theme.apply(app)
    test_registration()
    test_flow(app)
    test_cancel(app)
    print("\n" + ("全部通过" if not FAILS else f"失败 {len(FAILS)}: {FAILS}"))
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
