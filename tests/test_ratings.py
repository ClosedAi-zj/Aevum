# -*- coding: utf-8 -*-
"""评级与筛选（R01）：星级落盘、缩略图上画得出来、筛得动、导出真只导筛中的。

逻辑本身很简单，容易出错的是接线：筛选必须是视图层的（帧号不能变）、换个
Ratings 实例还得读得回同一份、导出清单要跟着勾选走。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_ratings.py
"""
import os
import shutil
import sys
import time

import cv2                              # noqa: E402
import numpy as np                      # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "acceptance"))
sys.path.insert(0, HERE)
TMP = os.path.join(HERE, "_tmp", "ratings")

from PySide6 import QtCore, QtGui, QtWidgets    # noqa: E402

from reference import qa as qa_mod                   # noqa: E402
from reference.gui import prefs, theme, workbench    # noqa: E402
from reference.gui.grader import developer           # noqa: E402
from reference.gui.grader import ratings as R        # noqa: E402
from test_qa import normal, write_set            # noqa: E402

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
    t0 = time.time()
    while (wb._job is not None and wb._job.isRunning()
           and time.time() - t0 < timeout):
        pump(app, 60)
    pump(app, 250)
    return wb._job is None or not wb._job.isRunning()


def make_series(d, bad=(12, 20, 25)):
    """30 帧 2% 渐变 + 全黑 / 虚焦 / 过曝各一帧（和 test_qa 同一套素材）。"""
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


def key_press(widget, app, key):
    QtWidgets.QApplication.sendEvent(
        widget, QtGui.QKeyEvent(QtCore.QEvent.Type.KeyPress, key,
                                QtCore.Qt.KeyboardModifier.NoModifier))
    pump(app, 60)


def select_rows(browser, rows):
    browser.list.clearSelection()
    for r in rows:
        browser.list.item(r).setSelected(True)


def combo_row(combo, data):
    for i in range(combo.count()):
        if combo.itemData(i) == data:
            return i
    return -1


def base(p):
    return os.path.basename(str(p))


def _gold_rows(path):
    """截图里每一行有多少金色像素（星级和色标同属一个色族）。

    只看代码证明不了标记真在屏幕上——星级一旦被视口裁掉，金色就只剩贴底的半
    截。按行给计数，调用方既能看总量也能看某一条带。星色画在噪声缩略图上会被
    反锯齿和描边拉暗，所以按色相判而不是硬比 #f2c14e；cv2 读不了中文名，像素
    走 QImage。
    """
    img = QtGui.QImage(path)
    if img.isNull():
        return 0, np.zeros(1, dtype=int)
    rgb = img.convertToFormat(QtGui.QImage.Format.Format_RGB32)
    buf = np.frombuffer(rgb.constBits(), dtype=np.uint8,
                        count=rgb.bytesPerLine() * rgb.height())
    px = buf.reshape(rgb.height(), rgb.bytesPerLine() // 4, 4)[:, :rgb.width()]
    r = px[:, :, 2].astype(int)
    g = px[:, :, 1].astype(int)
    b = px[:, :, 0].astype(int)
    return rgb.height(), ((r > 130) & (r > g + 20) & (g > b + 30)).sum(1)


# ---------------------------------------------------------------------- 数据模型
def test_model():
    d = os.path.join(TMP, "model")
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    r = R.Ratings(d)
    r.set("IMG_001.CR3", 5, "green")
    r.set(os.path.join(d, "sub", "IMG_002.jpg"), 3, "red")
    r.set("IMG_003.jpg", 0, "none")
    check("按文件名取回星级", r.get("/anywhere/IMG_001.CR3") == (5, "green"),
          str(r.get("IMG_001.CR3")))
    check("清零的那帧不留条目", "IMG_003.jpg" not in r.data, str(r.data))
    raw = open(os.path.join(d, R.FILENAME), encoding="utf-8").read()
    check("落盘就在序列文件夹里",
          os.path.isfile(os.path.join(d, R.FILENAME))
          and '"IMG_001.CR3"' in raw and '"stars":5' in raw, raw[:60])
    check("重开读得回来", R.Ratings(d).get("IMG_002.jpg") == (3, "red"),
          str(R.Ratings(d).get("IMG_002.jpg")))
    check("写完不留 .tmp",
          not [f for f in os.listdir(d) if f.endswith(".tmp")],
          str(os.listdir(d)))

    r2 = R.Ratings(d)
    r2.set("IMG_004.jpg", 99, "chartreuse")
    check("星级夹到 5、认不出的色标当无",
          r2.get("IMG_004.jpg") == (5, "none"), str(r2.get("IMG_004.jpg")))
    r2.set("IMG_004.jpg", -1)
    check("负数当 0 星并删条目", "IMG_004.jpg" not in r2.data, str(r2.data))

    open(os.path.join(d, R.FILENAME), "w", encoding="utf-8").write("{坏一半")
    r3 = R.Ratings(d)
    check("坏文件只当没评级",
          r3.get("IMG_001.CR3") == (0, "none") and not r3.data, str(r3.data))

    r4 = R.Ratings(d)
    r4.set_many(["A.jpg", "B.jpg"], 4, "blue")
    check("批量打星都生效", r4.get("A.jpg") == (4, "blue")
          and r4.get("B.jpg") == (4, "blue"), str(r4.data))
    r4.apply("A.jpg", None, "purple")     # 只改色标，星级不动
    r4.save()
    check("色标能单独改", R.Ratings(d).get("A.jpg") == (4, "purple"),
          str(R.Ratings(d).get("A.jpg")))

    loose = R.Ratings("")
    loose.set("x.jpg", 2)
    check("没有文件夹时只在内存里",
          loose.path is None and loose.get("x.jpg") == (2, "none"),
          str(loose.path))


# ---------------------------------------------------------------------- 界面链路
def test_gui():
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

    seq_dir = os.path.join(TMP, "seq")
    files = make_series(seq_dir)
    wb = workbench.Workbench([developer.Developer])
    wb.resize(1400, 860)
    wb.show()
    pump(app, 150)
    w = wb.module(developer.Developer.name)
    w.set_media_list(files)
    pump(app, 200)
    w.set_sub_mode("图库")
    pump(app, 300)
    gb = w.grid_browser
    # 预览完成时外壳也会写状态栏，所以要听的是调色台报出去的那句，不是终态
    seen_status = []
    real_status = w.status

    def record_status(text):
        seen_status.append(text)
        real_status(text)

    w.status = record_status

    # 缩略图在后台生成，画星级得先等图标就位
    t0 = time.time()
    while time.time() - t0 < 120:
        if all(not gb.list.item(r).icon().isNull()
               for r in range(gb.list.count())):
            break
        pump(app, 100)
    check("网格页显示全部帧", gb.list.count() == 30 and len(gb.paths) == 30,
          f"{gb.list.count()} 项")
    check("缩略图都出来了",
          all(not gb.list.item(r).icon().isNull()
              for r in range(gb.list.count())))
    check("评级表挂上两个浏览器",
          gb.ratings is w.browser.ratings
          and os.path.dirname(gb.ratings.path) == seq_dir,
          str(gb.ratings.path))

    # ---- 键盘打星 / 清零
    gb.list.setCurrentRow(2)
    key_press(gb.list, app, QtCore.Qt.Key.Key_4)
    check("按 4 打上四星", gb.rating_for(files[2]) == (4, "none"),
          str(gb.rating_for(files[2])))
    check("评级写进序列文件夹",
          os.path.isfile(os.path.join(seq_dir, R.FILENAME)))
    check("重开还是四星", R.Ratings(seq_dir).get(files[2]) == (4, "none"),
          str(R.Ratings(seq_dir).get(files[2])))
    key_press(gb.list, app, QtCore.Qt.Key.Key_0)
    check("按 0 清零", gb.rating_for(files[2]) == (0, "none")
          and "clip_0002.jpg" not in gb.ratings.data, str(gb.ratings.data))
    key_press(gb.list, app, QtCore.Qt.Key.Key_7)
    check("不在 0~5 的键不改评级", gb.rating_for(files[2]) == (0, "none"))
    key_press(gb.list, app, QtCore.Qt.Key.Key_4)

    # ---- 右键菜单结构 + 色标
    menu = gb.build_rating_menu()
    titles = [a.text() for a in menu.actions()]
    subs = [a.menu() for a in menu.actions() if a.menu() is not None]
    sub_texts = [t for s in subs for t in (a.text() for a in s.actions())]
    check("菜单有星级 / 色标 / 清除",
          all(t in titles for t in ("星级", "色标", "清除评级")), str(titles))
    check("星级六档、色标六色",
          sum(1 for t in sub_texts if t.endswith(" 星")) == 5
          and "清除星级" in sub_texts
          and all(c in sub_texts for c in ("红", "黄", "绿", "蓝", "紫", "无")),
          str(sub_texts))
    # 真的点一下「蓝」：菜单项连的就是批量上色标
    blue = next(a for s in subs for a in s.actions() if a.text() == "蓝")
    blue.trigger()
    pump(app, 80)
    check("菜单选色标生效", gb.rating_for(files[2]) == (4, "blue"),
          str(gb.rating_for(files[2])))
    menu.deleteLater()
    gb.rate(None, "green")
    check("色标能单独改", gb.rating_for(files[2]) == (4, "green"),
          str(gb.rating_for(files[2])))

    # ---- 多选批量
    select_rows(gb, (11, 12, 13))
    n = gb.rate(5)
    check("多选批量打星", n == 3 and all(gb.rating_for(files[i])[0] == 5
                                        for i in (11, 12, 13)), str(n))
    check("没选中的帧不受影响", gb.rating_for(files[5]) == (0, "none"))
    check("状态栏回报刚打的这几帧",
          any(s.endswith("5 星") and "3 帧" in s for s in seen_status),
          str(seen_status[-2:]))

    # ---- 星级筛选（只动视图）
    w.fl_stars.setValue(5)
    pump(app, 80)
    expect5 = [p for p in files if gb.ratings.stars(p) >= 5]
    check("筛出五星帧", gb.list.count() == 3
          and gb.filtered_paths() == expect5,
          f"{gb.list.count()} 项 / 期望 {len(expect5)}")
    check("筛选不动全量名单",
          gb.paths == files and len(w.photo_items()) == 30,
          f"{len(gb.paths)} 帧")
    check("顶栏计数改成筛出/总数", w.top_hint() == "3 / 30 张", w.top_hint())
    check("导出勾选跟着说明筛出几帧",
          w.ex_filtered.isEnabled() and "筛出 3 帧" in w.ex_filtered.toolTip(),
          w.ex_filtered.toolTip())

    # 视图第 2 行 → 原始帧号 12（不是视图行号 2）
    got = []
    gb.picked.connect(got.append)
    it = gb.list.item(1)
    check("视图行映射回原始帧号", gb._path_index(it) == 12,
          str(gb._path_index(it)))
    gb.list.itemClicked.emit(it)
    pump(app, 120)
    check("点击回报的是原始帧号", got == [12] and w.src_path == files[12],
          f"{got} · {base(w.src_path)}")
    gb.picked.disconnect(got.append)

    # ---- 筛着的时候打分：重建列表不能把选中一起抹掉
    select_rows(gb, (0, 1, 2))
    check("批量选中三帧", len(gb.selected_paths()) == 3)
    gb.rate(5)                       # 星级没变，但带筛选就会走重建
    pump(app, 80)
    check("筛选下打分后选中还在", gb.selected_paths() == expect5,
          str([base(p) for p in gb.selected_paths()]))
    check("重建后还是三帧", gb.list.count() == 3, str(gb.list.count()))
    select_rows(gb, (0, 1))
    gb.rate(2)                       # 这两帧掉出五星筛选
    pump(app, 80)
    check("掉出筛选的帧不再占位", gb.filtered_paths() == [files[13]],
          str([base(p) for p in gb.filtered_paths()]))
    check("全掉出时状态栏仍报刚打的那两帧",
          any(s.endswith("2 星") and "2 帧" in s for s in seen_status),
          str(seen_status[-2:]))
    w.clear_filter()
    pump(app, 80)
    select_rows(gb, (11, 12))        # 复原，后面的断言按三帧五星走
    gb.rate(5)
    w.fl_stars.setValue(5)
    pump(app, 80)
    check("复原后筛选又是三帧", gb.filtered_paths() == expect5,
          str([base(p) for p in gb.filtered_paths()]))

    # ---- 色标筛选与叠加
    w.fl_stars.setValue(0)
    w.fl_label.setCurrentIndex(combo_row(w.fl_label, "green"))
    pump(app, 80)
    check("按色标筛", gb.filtered_paths() == [files[2]],
          str([base(p) for p in gb.filtered_paths()]))
    select_rows(gb, (0,))          # 筛着的时候第 0 行就是唯一可见的 clip_0002
    gb.rate(4, "green")
    pump(app, 80)
    check("状态栏报刚打的那一帧",
          any(s == "第 3 帧 · 4 星 · 绿标" for s in seen_status),
          str(seen_status[-2:]))
    check("筛选状态下打分只落在可见帧",
          gb.rating_for(files[0]) == (0, "none")
          and gb.rating_for(files[2]) == (4, "green"), str(gb.ratings.data))
    w.clear_filter()
    select_rows(gb, (0,))
    gb.rate(4, "green")
    w.fl_stars.setValue(4)
    w.fl_label.setCurrentIndex(combo_row(w.fl_label, "green"))
    pump(app, 80)
    check("星级 + 色标叠加", sorted(gb.filtered_paths())
          == sorted([files[0], files[2]]),
          str([base(p) for p in gb.filtered_paths()]))
    w.clear_filter()
    pump(app, 80)
    check("清除筛选全回来",
          gb.list.count() == 30 and not gb.filter_spec(),
          str(gb.list.count()))
    check("没筛选时导出勾选置灰", not w.ex_filtered.isEnabled())

    # ---- 异常筛选：体检之前必须置灰
    check("没体检不给筛异常", not w.fl_qa.isEnabled()
          and "先跑一次序列体检" in w.fl_qa.toolTip(), w.fl_qa.toolTip())
    w.open_qa()
    dlg = w._qa_dlg
    if dlg is None:
        check("体检对话框打开", False)
        return 1
    if not wait_job(app, wb):
        check("体检任务超时", False)
        return 1
    check("体检结论进了调色台", len(w._qa_kinds) == 3,
          str(len(w._qa_kinds)))
    check("体检后能筛异常", w.fl_qa.isEnabled(), w.fl_qa.toolTip())
    check("面板提示能筛异常", "只看异常" in w.lb_qa.text(), w.lb_qa.text())
    w.fl_qa.setChecked(True)
    pump(app, 100)
    check("只看异常筛出三帧", sorted(gb.filtered_paths())
          == sorted([files[12], files[20], files[25]]),
          str([base(p) for p in gb.filtered_paths()]))
    w.fl_kind.setCurrentIndex(combo_row(w.fl_kind, "blur"))
    pump(app, 100)
    check("按异常类型再筛", gb.filtered_paths() == [files[20]],
          str([qa_mod.label(k) for k in
               w._qa_kinds[os.path.normcase(os.path.normpath(files[20]))]]))
    w.clear_filter()
    pump(app, 100)

    # ---- 导出尊重筛选
    w.clear_qa()
    dlg.reject()
    pump(app, 120)
    warns = []
    w.warn = lambda text, title=None: warns.append(text)
    out1 = os.path.join(TMP, "out_filtered")
    out2 = os.path.join(TMP, "out_all")
    out3 = os.path.join(TMP, "out_empty")
    for x in (out1, out2, out3):
        os.makedirs(x, exist_ok=True)
    real_dir = QtWidgets.QFileDialog.getExistingDirectory
    pick = {"d": out1}
    QtWidgets.QFileDialog.getExistingDirectory = staticmethod(
        lambda *a, **k: pick["d"])
    try:
        select_rows(gb, (11, 12, 13))
        gb.rate(3, "yellow")
        w.fl_label.setCurrentIndex(combo_row(w.fl_label, "yellow"))
        pump(app, 80)
        check("筛出三帧黄色标",
              gb.filtered_paths() == [files[11], files[12], files[13]],
              str([base(p) for p in gb.filtered_paths()]))
        w.ex_filtered.setChecked(True)
        w.export_batch()
        if not wait_job(app, wb):
            check("筛选导出超时", False)
        else:
            got = sorted(f for f in os.listdir(out1) if f.endswith(".jpg"))
            check("只导出筛选出来的三帧",
                  got == ["clip_0011.jpg", "clip_0012.jpg", "clip_0013.jpg"],
                  str(got))
        w.ex_filtered.setChecked(False)
        pick["d"] = out2
        w.export_batch()
        if not wait_job(app, wb):
            check("全量导出超时", False)
        else:
            n = len([f for f in os.listdir(out2) if f.endswith(".jpg")])
            check("取消勾选导全部 30 帧", n == 30, str(n))
        # 筛到空：提示一句就收，不能默默发一个 0 帧的任务
        w.fl_stars.setValue(5)
        w.fl_label.setCurrentIndex(combo_row(w.fl_label, "purple"))
        w.ex_filtered.setChecked(True)
        pick["d"] = out3
        check("这组条件一张都不剩", gb.filtered_paths() == [],
              str(gb.filtered_paths()))
        w.export_batch()
        pump(app, 120)
        check("空筛选有提示且不发任务",
              bool(warns) and "筛选" in warns[-1]
              and not os.listdir(out3), str(warns[-2:]))
    finally:
        QtWidgets.QFileDialog.getExistingDirectory = real_dir

    # ---- 换素材对齐到新文件夹
    d2 = os.path.join(TMP, "seq2")
    files2 = make_series(d2, bad=())
    w.set_media_list(files2)
    pump(app, 250)
    check("换序列换到新的评级文件",
          os.path.dirname(gb.ratings.path) == d2
          and gb.rating_for(files[2]) == (0, "none"), str(gb.ratings.path))
    check("换序列后异常筛选置灰", not w.fl_qa.isEnabled(), w.lb_qa.text())

    # 截图：筛选条 + 网格上的星级与色标
    w.clear_filter()
    for i in range(1, 6):
        select_rows(gb, (i,))
        gb.rate(i, (None, "red", "yellow", "green", "blue", "purple")[i])
    select_rows(gb, (7,))
    gb.rate(0, "none")
    pump(app, 250)
    w.grid_host.grab().save(os.path.join(ROOT, "shots", "shot_评级筛选.png"))
    check("截图已存",
          os.path.isfile(os.path.join(ROOT, "shots", "shot_评级筛选.png")))
    gh, gcounts = _gold_rows(os.path.join(ROOT, "shots", "shot_评级筛选.png"))
    # 网格是多行的，星级散在各格右下，只能按金色总量判；胶片条单行才分带
    check("网格里星级画得出来", int(gcounts.sum()) >= 500,
          f"金色 {int(gcounts.sum())} px / 高 {gh}")

    # 显影底部的胶片条用同一套绘制：标记不能只在全屏网格里才有
    w.set_sub_mode("显影")
    strip = w.browser
    t0 = time.time()
    while time.time() - t0 < 120:
        if all(not strip.list.item(r).icon().isNull()
               for r in range(strip.list.count())):
            break
        pump(app, 100)
    check("胶片条也显示全量", strip.list.count() == 30, str(strip.list.count()))
    select_rows(strip, (2,))
    strip.rate(5, "green")
    check("胶片条上也能打星", strip.rating_for(files2[2]) == (5, "green"),
          str(strip.rating_for(files2[2])))
    for i in (5, 8, 11):
        select_rows(strip, (i,))
        strip.rate(i - 4, "yellow")
    pump(app, 200)
    # 亮度曲线一出来就会抢占高度：视口低于一格时，缩略图下沿和星级一起被裁掉
    t0 = time.time()
    while time.time() - t0 < 60 and not strip.curve.isVisible():
        pump(app, 100)
    vp = strip.list.viewport().height()
    cell = strip.STRIP_CELL.height()
    check("曲线出来后仍装得下一整格", vp >= cell,
          f"视口 {vp} / 格子 {cell} · 曲线"
          f"{'可见' if strip.curve.isVisible() else '未出现'}")
    shot = os.path.join(ROOT, "shots", "shot_评级胶片条.png")
    strip.grab().save(shot)
    check("胶片条截图已存", os.path.isfile(shot))
    h, counts = _gold_rows(shot)
    band = counts[h // 2:]          # 下半带才是星级，上半是色标
    # 这里 15 颗星，反锯齿加描边后每颗只剩 ~19 px；被视口切掉时实测 180 px
    check("星级整颗画在缩略图上", int(band.sum()) >= 250,
          f"星级带 {int(band.sum())} px / {int((band > 0).sum())} 行 / 高 {h}")

    wb.close()
    pump(app, 150)
    return 0


def main():
    test_model()
    if test_gui():
        return 1
    print(f"\n{TOTAL[0] - len(FAILS)}/{TOTAL[0]} 通过"
          + (f"  失败: {FAILS}" if FAILS else ""))
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
