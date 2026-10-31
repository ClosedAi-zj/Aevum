# -*- coding: utf-8 -*-
"""渲染队列（B1）：几段序列排成一队，各按各文件夹里的调色出成片。

这里只管"队列"这件事特有的东西：排队与去重、写进文件、每段读自己的旁车、
中间帧目录的建/删/续、取消后接着渲、改过调色旧帧作废、单段坏了不拖死整队。
逐帧落盘与断点续跑本身在 tests/test_resume.py 里验。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_queue.py
"""
import os
import shutil
import sys
import time

import cv2
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "acceptance"))
sys.path.insert(0, HERE)
TMP = os.path.join(HERE, "_tmp", "queue")
SHOTS = os.path.join(ROOT, "shots")

from PySide6 import QtCore, QtWidgets         # noqa: E402

from reference.gui import prefs, theme, workbench   # noqa: E402
from reference.gui.grader import developer, renderq   # noqa: E402

FAILS = []
TOTAL = [0]
WARNS = []
FPS = "12"
SIDE = "320"


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


def wait_idle(app, wb, timeout=300):
    """等外壳那条全局任务结束（一项要渲帧 + 编码，给足时间）。"""
    t0 = time.time()
    while wb._job is not None and wb._job.isRunning() and time.time() - t0 < timeout:
        pump(app, 60)
    pump(app, 200)
    return wb._job is None or not wb._job.isRunning()


def make_series(name, n, level=100):
    """一段 16:9 小序列：整体亮度 level，帧间 2% 渐变（日转夜的缩影）。"""
    d = os.path.join(TMP, name)
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    rng = np.random.default_rng(7)
    low = rng.random((6, 10)).astype(np.float32)
    paths = []
    for i in range(n):
        p = os.path.join(d, f"clip_{i:04d}.jpg")
        im = (cv2.resize(low, (160, 90), interpolation=cv2.INTER_CUBIC)
              * 30 + level) * 2 ** (i * 0.02)
        cv2.imwrite(p, cv2.cvtColor(np.clip(im, 0, 255).astype(np.uint8),
                                    cv2.COLOR_GRAY2BGR),
                    [cv2.IMWRITE_JPEG_QUALITY, 96])
        paths.append(p)
    return paths


def source_mean(paths):
    """这一批源帧的平均亮度，用来对照成片是变亮了还是变暗了。"""
    vals = [cv2.cvtColor(cv2.imread(p, cv2.IMREAD_COLOR),
                         cv2.COLOR_BGR2GRAY).mean() for p in paths]
    return sum(vals) / max(len(vals), 1)


def video_stats(path):
    """成片：帧数、尺寸、平均亮度 —— 顺带确认这片子真的能播。"""
    cap = cv2.VideoCapture(path)
    n, total, size = 0, 0.0, (0, 0)
    while True:
        ok, fr = cap.read()
        if not ok:
            break
        if not n:
            size = (fr.shape[1], fr.shape[0])
        total += cv2.cvtColor(fr, cv2.COLOR_BGR2GRAY).mean()
        n += 1
    cap.release()
    return n, size, total / max(n, 1)


def out_of(name):
    """队列的默认成片名：<序列目录的父目录>/<目录名>_<帧率>fps_<长边>.mp4。"""
    return os.path.join(TMP, f"{name}_{float(FPS):g}fps_{SIDE}.mp4")


def row_of(w, name):
    folder = os.path.normpath(os.path.join(TMP, name))
    for it in w._queue:
        if it["folder"] == folder:
            return it
    return None


def spec(w, keep=False):
    """设队列那套成片规格，和用户在界面上点选等价。"""
    w.q_fps.setCurrentText(FPS)
    w.q_side.setCurrentText(SIDE)
    w.q_aspect.setCurrentIndex(1)          # 16:9
    w.q_codec.setCurrentIndex(1)           # H.264，测试要快
    w.ex_preset.setCurrentIndex(max(w.ex_preset.findData("veryfast"), 0))
    w.ex_crf.setValue(23)
    w.q_keep.setChecked(keep)


def add(w, app, wb, files, exposure):
    """打开这段序列、给它一套调色、排进队列 —— 用户在界面上就是这三下。"""
    w.set_media_list(files)
    pump(app, 120)
    wait_idle(app, wb)
    w.store.set("exposure", exposure)
    pump(app, 80)
    w.queue_add()
    pump(app, 60)


def test_ui_and_guards(app, wb, w):
    titles = [lb.text() for lb in w.dock_export.findChildren(QtWidgets.QLabel)]
    check("导出页有渲染队列一节", "渲染队列（多序列）" in titles)
    check("队列挂了完整讲解", w.lb_queue.toolTip().startswith("【多序列渲染队列】"),
          w.lb_queue.toolTip()[:20])
    check("加入按钮挂了讲解", w.btn_q_add.toolTip().startswith("【加入当前序列】"),
          w.btn_q_add.toolTip()[:20])
    check("空队列没有行", w.lb_queue.count() == 0 and not w._queue)
    check("空队列不给点渲染", not w.btn_queue.isEnabled())

    single = make_series("solo", 1)
    w.set_media_list(single)
    pump(app, 120)
    w.queue_add()
    check("单帧排队被拦住并说明原因", not w._queue and len(WARNS) == 1
          and "图片序列" in WARNS[0], WARNS[0][:36] if WARNS else "没提示")

    two = make_series("wide", 4)
    w.set_media_list(two)
    pump(app, 120)
    wait_idle(app, wb)
    w.queue_add()
    check("两帧就肯排", len(w._queue) == 1, str(len(w._queue)))
    w.queue_clear()


def test_enqueue(app, wb, w):
    a = make_series("day", 12, level=70)
    b = make_series("night", 9, level=140)
    spec(w)
    add(w, app, wb, a, 1.0)
    check("排进一段", len(w._queue) == 1
          and row_of(w, "day")["status"] == renderq.PENDING)
    check("行文字报帧数与规格",
          "12 帧" in w.lb_queue.item(0).text()
          and "12fps" in w.lb_queue.item(0).text(), w.lb_queue.item(0).text())
    check("成片路径跟着目录名走", row_of(w, "day")["out"] == out_of("day"),
          row_of(w, "day")["out"])
    w.queue_add()
    check("同目录重复加入只留一行", len(w._queue) == 1, str(len(w._queue)))

    add(w, app, wb, b, -1.0)
    check("两段都在队里", len(w._queue) == 2 and w.lb_queue.count() == 2)
    check("每段记住自己的帧数",
          [row_of(w, "day")["frames"], row_of(w, "night")["frames"]] == [12, 9],
          str([row_of(w, "day")["frames"], row_of(w, "night")["frames"]]))
    saved = renderq.load()
    check("队列写进了文件", len(saved) == 2
          and {r["folder"] for r in saved} == {os.path.normpath(os.path.join(
              TMP, "day")), os.path.normpath(os.path.join(TMP, "night"))},
          str([os.path.basename(r["folder"]) for r in saved]))

    n0 = len(w._queue)
    w.lb_queue.setCurrentRow(0)
    w.queue_remove()
    check("移出删的是选中那行", len(w._queue) == n0 - 1
          and row_of(w, "day") is None and row_of(w, "night"),
          str([r["folder"] for r in w._queue]))
    add(w, app, wb, a, 1.0)
    check("重新排回两段", len(w._queue) == 2)
    return a, b


def test_render_two(app, wb, w, a, b):
    w.render_queue()
    check("队列期间按钮禁用",
          not w.btn_q_add.isEnabled() and not w.btn_queue.isEnabled())
    if not wait_idle(app, wb):
        check("队列跑完", False, "超时")
        return False
    outs = [out_of("day"), out_of("night")]
    check("两段成片都渲出来", all(os.path.isfile(p) for p in outs), " / ".join(outs))
    if not all(os.path.isfile(p) for p in outs):
        return False
    na, sa, ma = video_stats(outs[0])
    nb, sb, mb = video_stats(outs[1])
    check("day 段帧数对得上", na == len(a), f"{na}/{len(a)}")
    check("night 段帧数对得上", nb == len(b), f"{nb}/{len(b)}")
    check("按 320 长边 16:9 出片", sa == (320, 180) and sb == (320, 180),
          f"{sa} {sb}")
    # 调色 +1 档到成片是"亮了不少"，不是恰好 2 倍：色调曲线会把高光往下压
    ra, rb = ma / source_mean(a), mb / source_mean(b)
    check("day 用自己那套（+1 档变亮）", ra > 1.2,
          f"源 {source_mean(a):.0f} → 片 {ma:.0f}（{ra:.2f}×）")
    check("night 用自己那套（-1 档变暗）", rb < 0.85,
          f"源 {source_mean(b):.0f} → 片 {mb:.0f}（{rb:.2f}×）")
    check("两段各按各的参数，没被一套抹平", ra / rb > 1.4,
          f"{ra:.2f}× vs {rb:.2f}×")
    check("默认渲完就清中间帧",
          all(not os.path.exists(renderq.frames_dir(p)) for p in outs))
    check("两行都标完成",
          row_of(w, "day")["status"] == renderq.DONE
          and row_of(w, "night")["status"] == renderq.DONE,
          str([r["status"] for r in w._queue]))
    check("说明写了清帧", "中间帧已清" in row_of(w, "day")["note"],
          row_of(w, "day")["note"])
    check("都渲完后不再给点", not w.btn_queue.isEnabled())
    check("状态栏报了几段", "队列渲成 2 段" in wb.lb_status.text(),
          wb.lb_status.text())

    # 已完成的段再点渲染不该重跑
    made = os.path.getmtime(outs[0])
    w.render_queue()
    check("没有待渲的段就不动手", wait_idle(app, wb)
          and "队列里没有待渲的段" in wb.lb_status.text(), wb.lb_status.text())
    check("成片没被动过", os.path.getmtime(outs[0]) == made)
    return True


def test_cancel_resume(app, wb, w):
    """取消：半成品不留，帧与进度留着；参数没改就接着渲剩下的。"""
    c = make_series("hold", 150, level=100)
    spec(w, keep=True)
    add(w, app, wb, c, 0.4)
    out, fd = out_of("hold"), renderq.frames_dir(out_of("hold"))
    w.render_queue()
    t0 = time.time()
    while time.time() - t0 < 120:
        pump(app, 40)
        if os.path.isdir(fd) and len([f for f in os.listdir(fd)
                                      if f.endswith(".jpg")]) >= 5:
            break
    wb.cancel_job()
    check("取消后队列停下", wait_idle(app, wb))
    made = len([f for f in os.listdir(fd) if f.endswith(".jpg")]) \
        if os.path.isdir(fd) else 0
    check("取消时不写半成品", not os.path.exists(out))
    check("取消时中间帧留着", 0 < made < 150, f"已渲 {made} 帧")
    check("取消时记下断点", os.path.isfile(renderq.progress_path(fd)))
    check("这一行标失败并说已取消",
          row_of(w, "hold")["status"] == renderq.FAILED
          and "已取消" in row_of(w, "hold")["note"], row_of(w, "hold")["note"])
    check("取消也报进度还在", "接着渲" in wb.lb_status.text(), wb.lb_status.text())

    w.render_queue()
    check("续跑跑完", wait_idle(app, wb))
    check("续跑日志说跳过", any("断点续跑" in t for t in w._job_log),
          next((t for t in w._job_log if "断点" in t), "没有这句"))
    check("续跑把这段渲成片", os.path.isfile(out))
    if os.path.isfile(out):
        n, _s, _m = video_stats(out)
        check("续跑拼出的帧数完整", n == 150, f"{n}/150")
    check("勾了保留就留住中间帧", os.path.isdir(fd), fd)
    check("渲完删掉断点文件", not os.path.isfile(renderq.progress_path(fd)))
    check("说明写了留帧", "中间帧留在" in row_of(w, "hold")["note"],
          row_of(w, "hold")["note"])
    return c, out, fd


def test_grading_change(app, wb, w, c, out, fd):
    """改过调色：旧中间帧不作数，整批重渲（不然新旧帧混在一部片里）。"""
    first = os.path.join(fd, "clip_0000.jpg")
    before = os.path.getmtime(first)
    os.utime(first, (before - 5, before - 5))
    spec(w, keep=True)
    add(w, app, wb, c, -0.8)
    check("重排队把状态退回待渲",
          row_of(w, "hold")["status"] == renderq.PENDING)
    os.remove(out)
    w.render_queue()
    check("重渲跑完", wait_idle(app, wb))
    check("改了调色说一声旧帧作废",
          any("全部重渲" in t for t in w._job_log),
          next((t for t in w._job_log if "重渲" in t), "没有这句"))
    check("第一帧真的重画过", os.path.getmtime(first) > before,
          f"{os.path.getmtime(first) - before:+.0f}s")
    check("重渲用的是新那套（变暗）", os.path.isfile(out), out)
    if os.path.isfile(out):
        n, _s, mean = video_stats(out)
        check("新帧比旧帧暗", mean < source_mean(c) * 0.8,
              f"源 {source_mean(c):.0f} → 片 {mean:.0f}")
        check("帧数还是整段", n == 150, f"{n}/150")
    check("这一段又标回完成", row_of(w, "hold")["status"] == renderq.DONE)


def test_bad_row_isolated(app, wb, w):
    """坏一段不拖死整队：它自己记失败，后面的照常出片。"""
    g = make_series("broken", 6, level=100)
    h = make_series("good", 8, level=110)
    spec(w, keep=False)
    add(w, app, wb, g, 0.3)
    add(w, app, wb, h, 0.3)
    # 排完队才把它的图换成坏字节：排队时看得见，渲的时候才发现读不出来
    for p in g:
        with open(p, "wb") as f:
            f.write(b"not an image at all")
    w.render_queue()
    check("混着坏段的队列跑完", wait_idle(app, wb))
    check("坏段记失败并说原因",
          row_of(w, "broken")["status"] == renderq.FAILED
          and "不足两张" in row_of(w, "broken")["note"],
          row_of(w, "broken")["note"])
    check("后面那段照样渲成",
          row_of(w, "good")["status"] == renderq.DONE
          and os.path.isfile(out_of("good")), out_of("good"))
    check("失败数报进状态栏", "1/2 段没渲成" in wb.lb_status.text(),
          wb.lb_status.text())

    # 目录整个没了：连渲染计划都建不出来，队列照样说得清
    shutil.rmtree(os.path.dirname(g[0]), ignore_errors=True)
    row_of(w, "broken")["status"] = renderq.PENDING
    renderq.save(w._queue)
    w._queue_refresh()
    w.render_queue()
    check("缺目录那段单独失败",
          row_of(w, "broken")["status"] == renderq.FAILED
          and "目录不存在" in row_of(w, "broken")["note"],
          row_of(w, "broken")["note"])
    check("建不出计划时不开工", "都渲不了" in wb.lb_status.text(),
          wb.lb_status.text())


def test_clear_keeps_files(app, wb, w):
    mp4 = [f for f in os.listdir(TMP) if f.endswith(".mp4")]
    check("渲好的成片不止一段", len(mp4) >= 3, str(mp4))
    w.queue_clear()
    check("清空只清名单", not w._queue and w.lb_queue.count() == 0
          and not renderq.load())
    check("清空不动渲好的文件",
          [f for f in os.listdir(TMP) if f.endswith(".mp4")] == mp4, str(mp4))
    check("清空后不给点", not w.btn_queue.isEnabled())
    w.render_queue()
    check("空队列点下去只说一句", "队列里没有待渲的段" in wb.lb_status.text(),
          wb.lb_status.text())


def scroll_to(w, widget, pad=24):
    """把导出面板滚到某一节：不滚过去，截图里根本看不到它。"""
    sa = widget.parentWidget()
    while sa is not None and not isinstance(sa, QtWidgets.QScrollArea):
        sa = sa.parentWidget()
    if sa is None:
        return False
    sa.verticalScrollBar().setValue(
        max(sa.widget().mapTo(sa, widget.pos()).y() - pad, 0))
    return True


def shot(app, wb, w):
    """截图要有两段排好的队列，界面上看得见行（渲好的 ✓ 与待渲的 ·）。"""
    spec(w)
    add(w, app, wb, make_series("shotA", 12, level=80), 0.8)
    b = make_series("shotB", 9, level=130)
    add(w, app, wb, b, -0.5)
    row_of(w, "shotA")["status"] = renderq.DONE
    row_of(w, "shotA")["note"] = "中间帧已清"
    w._queue_refresh()
    check("导出面板能滚到队列那一节", scroll_to(w, w.lb_queue))
    pump(app, 250)
    os.makedirs(SHOTS, exist_ok=True)
    w.dock_export.grab().save(os.path.join(SHOTS, "shot_渲染队列面板.png"))
    wb.grab().save(os.path.join(SHOTS, "shot_渲染队列.png"))
    check("截图里有队列面板",
          os.path.getsize(os.path.join(SHOTS, "shot_渲染队列面板.png")) > 20000)


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
    # 队列文件也指到临时目录：测试绝不能碰用户那份真队列
    renderq.PATH = os.path.join(TMP, "render_queue.json")
    QtWidgets.QMessageBox.information = staticmethod(lambda *a, **k: None)
    app = QtWidgets.QApplication.instance() or QtWidgets.QApplication(sys.argv)
    theme.apply(app)

    wb = workbench.Workbench([developer.Developer], restore=False)
    wb.resize(1400, 900)
    wb.show()
    pump(app, 150)
    w = wb.module(developer.Developer.name)
    w.warn = lambda text, title=None: WARNS.append(text)
    wb.set_module(w.name)
    w.set_sub_mode("导出")
    pump(app, 200)

    test_ui_and_guards(app, wb, w)
    a, b = test_enqueue(app, wb, w)
    if test_render_two(app, wb, w, a, b):
        c, out, fd = test_cancel_resume(app, wb, w)
        test_grading_change(app, wb, w, c, out, fd)
    test_bad_row_isolated(app, wb, w)
    test_clear_keeps_files(app, wb, w)
    shot(app, wb, w)

    wb.close()
    pump(app, 150)
    print(f"\n{TOTAL[0] - len(FAILS)}/{TOTAL[0]} 通过"
          + (f"  失败: {FAILS}" if FAILS else ""))
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
