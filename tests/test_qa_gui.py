# -*- coding: utf-8 -*-
"""序列体检的界面链路：表格里报对帧、勾选能排除、导出真的跳过、换素材自动清空。

判定本身在 tests/test_qa.py 里验，这里只管 Qt 这一侧的接线。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_qa_gui.py
"""
import os
import shutil
import sys
import time

import numpy as np                      # noqa: E402
import cv2                              # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, ROOT)
sys.path.insert(0, HERE)
TMP = os.path.join(HERE, "_tmp", "qa_gui")

from PySide6 import QtCore, QtWidgets    # noqa: E402

from lapse.gui import prefs, theme, workbench   # noqa: E402
from lapse.gui.grader import developer          # noqa: E402
from lapse.gui.grader import qa as qa_ui        # noqa: E402
from test_qa import normal, write_set          # noqa: E402

FAILS = []
TOTAL = [0]


def check(name, ok, detail=""):
    TOTAL[0] += 1
    print(f"[{'PASS' if ok else 'FAIL'}] {name}  {detail}", flush=True)
    if not ok:
        FAILS.append(name)


def pump(app, ms=200):
    t0 = time.time()
    while (time.time() - t0) * 1000 < ms:
        app.processEvents()
        QtCore.QThread.msleep(5)
    app.processEvents()


def wait_job(app, wb, timeout=180):
    """等外壳那条全局任务跑完（_job 结束后仍留着，只能看 isRunning）。"""
    t0 = time.time()
    while wb._job is not None and wb._job.isRunning() and time.time() - t0 < timeout:
        pump(app, 60)
    pump(app, 250)
    return wb._job is None or not wb._job.isRunning()


def make_series(d, bad=(12, 20, 25)):
    """和 tests/test_qa.py 同一套素材：2% 亮度渐变 + 全黑 / 虚焦 / 过曝各一帧。"""
    frames = normal(30, 200, 150)
    if 12 in bad:
        frames[12] = np.zeros_like(frames[12])
    if 20 in bad:
        frames[20] = cv2.GaussianBlur(frames[20], (0, 0), 8)
    if 25 in bad:
        frames[25] = np.clip(frames[25].astype(np.float32) * 3.0, 0,
                             255).astype(np.uint8)
    return write_set(d, [("clip_%04d.jpg" % i, f)
                         for i, f in enumerate(frames)])


def table_rows(dlg):
    out = []
    for row in range(dlg.table.rowCount()):
        it = dlg.table.item(row, 1)
        frame = int(it.data(QtCore.Qt.ItemDataRole.UserRole))
        kind = dlg.table.item(row, 2).text()
        checkable = bool(dlg.table.item(row, 0).flags()
                         & QtCore.Qt.ItemFlag.ItemIsUserCheckable)
        out.append((frame, kind, checkable))
    return out


def main():
    shutil.rmtree(TMP, ignore_errors=True)
    os.makedirs(TMP)
    ini = os.path.join(TMP, "settings")
    QtCore.QSettings.setDefaultFormat(QtCore.QSettings.Format.IniFormat)
    QtCore.QSettings.setPath(QtCore.QSettings.Format.IniFormat,
                             QtCore.QSettings.Scope.UserScope, ini)
    prefs.put("cache/dir", os.path.join(TMP, "cache"))
    prefs.put("plugins/dir", os.path.join(TMP, "plugins"))
    prefs.put("edits/dir", os.path.join(TMP, "edits"))
    app = QtWidgets.QApplication(sys.argv)
    theme.apply(app)

    files = make_series(os.path.join(TMP, "seq"))
    wb = workbench.Workbench([developer.Developer])
    wb.resize(1400, 860)
    wb.show()
    pump(app, 150)
    w = wb.module(developer.Developer.name)
    w.set_media_list(files)
    pump(app, 200)

    check("体检按钮就绪", w.btn_qa.isEnabled(),
          "需要至少两张图片")
    # 显影页的胶片条会自己测光；体检应当直接复用那份完整测量
    w.set_sub_mode("显影")
    t0 = time.time()
    while w.browser.luma_for(files) is None and time.time() - t0 < 90:
        pump(app, 100)
    cached = w.browser.luma_for(files)
    check("胶片条留存完整测量",
          cached is not None and len(cached) == len(files)
          and all(len(v) == 4 for v in cached), "体检不用重扫一遍")
    w.open_qa()
    dlg = w._qa_dlg
    check("体检对话框打开", dlg is not None)
    if dlg is None:
        return 1
    if not wait_job(app, wb):
        check("体检任务超时", False)
        return 1
    rows = table_rows(dlg)
    kinds = {f: k for f, k, _c in rows}
    check("表格报出三帧", sorted(kinds) == [12, 20, 25], str(rows))
    check("帧号按 1 起展示", dlg.table.item(0, 1).text().startswith("13 / 30"),
          dlg.table.item(0, 1).text())
    check("状态行有汇总", "发现 3 处" in dlg.lb_state.text(),
          dlg.lb_state.text())

    # 勾选排除
    dlg._check_all(True)
    dlg.grab().save(os.path.join(ROOT, "shots", "shot_序列体检.png"))
    ex = w.qa_excluded()
    check("勾选写进调色台", len(ex) == 3, str(sorted(ex))[:80])
    check("面板提示排除数", "已排除 3 帧" in w.lb_qa.text(), w.lb_qa.text())
    kept = w.apply_qa(w.photo_items())
    check("导出列表去掉这三帧",
          len(kept) == 27 and all(files[i] not in kept for i in (12, 20, 25)),
          f"{len(kept)}/30")

    dlg._check_all(False)
    check("全不选能清空", not w.qa_excluded(), str(sorted(w.qa_excluded()))[:60])
    check("清空后改报异常帧", "发现 3 帧异常" in w.lb_qa.text(), w.lb_qa.text())

    # 双击跳帧
    dlg._check_all(True)
    for row in range(dlg.table.rowCount()):
        if dlg.table.item(row, 1).data(QtCore.Qt.ItemDataRole.UserRole) == 20:
            dlg.table.setCurrentCell(row, 1)
            dlg._jump(dlg.table.item(row, 1))
            break
    check("双击跳到那一帧", w.src_path == files[20],
          str(w.src_path))

    # 缺口行不可勾选：换素材后旧结论作废，重新体检才对着新列表
    d2 = os.path.join(TMP, "gap")
    g2 = [p for i, p in enumerate(files) if i not in (5, 6, 7)]
    w.set_media_list(g2)
    pump(app, 150)
    check("换素材后排除自动清空", not w.qa_excluded(),
          str(sorted(w.qa_excluded()))[:60])
    check("换素材后旧表清空", dlg.table.rowCount() == 0,
          f"{dlg.table.rowCount()} 行")
    check("换素材后提示重新体检", "序列变了" in dlg.lb_state.text(),
          dlg.lb_state.text())
    dlg._check_all(True)
    check("作废的表勾不动", not w.qa_excluded(), str(w.qa_excluded()))

    dlg._start()
    if not wait_job(app, wb):
        check("第二次体检超时", False)
        return 1
    rows2 = table_rows(dlg)
    gaps = [r for r in rows2 if r[1] == "序列缺口"]
    check("缺口行报出来了", len(gaps) == 1 and gaps[0][0] == 5, str(gaps))
    check("缺口行不给勾选", gaps and not gaps[0][2], str(gaps))
    bad_rows = [r for r in rows2 if r[1] != "序列缺口"]
    check("缺口以外的行可勾选", all(r[2] for r in bad_rows), str(bad_rows))
    dlg.reject()
    pump(app, 100)
    check("关掉窗口后引用释放", w._qa_dlg is None, str(w._qa_dlg))

    # 太短的素材：按钮直接禁用
    w.set_media_list(files[:1])
    pump(app, 120)
    check("单帧不给体检", not w.btn_qa.isEnabled(), w.lb_qa.text())

    wb.close()
    pump(app, 150)
    print(f"\n{TOTAL[0] - len(FAILS)}/{TOTAL[0]} 通过"
          + (f"  失败: {FAILS}" if FAILS else ""))
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
