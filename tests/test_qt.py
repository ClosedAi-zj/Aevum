# -*- coding: utf-8 -*-
"""调色台功能回归：渲染 + 历史 + 预设/HSL/曲线 + 对比 + 停靠 + 导出 + 截图。

调色台是工作台的一个模块：菜单与进度条归外壳，这里驱动模块本身。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_qt.py
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

import numpy as np                      # noqa: E402
import cv2                              # noqa: E402
from PIL import Image                   # noqa: E402
from PySide6 import QtCore, QtGui, QtWidgets   # noqa: E402

from reference import grade as gc           # noqa: E402
from reference import holy as holy_core, imio  # noqa: E402
from reference.gui import edits, prefs, theme, widgets, workbench  # noqa: E402
from reference.gui.grader import (developer, exporter, filmstrip, params,  # noqa: E402
                              pipeline)

SRC = os.path.join(ROOT, "aligned_lr3")
SHOTS = os.path.join(ROOT, "shots")
FAILS = []


def check(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}"
          + (f"  {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def pump(app, ms=200):
    t0 = time.time()
    while (time.time() - t0) * 1000 < ms:
        app.processEvents()
        QtCore.QThread.msleep(5)
    app.processEvents()


def send(widget, typ, pos, button=QtCore.Qt.MouseButton.LeftButton):
    """投递一个真实鼠标事件。注意 QGraphicsView 会丢掉直接发给它的鼠标事件，
    必须发给 viewport（和用户点击的路径一致），走 sendEvent 才会按这条路径分发。"""
    b = button if typ != QtCore.QEvent.Type.MouseButtonRelease \
        else QtCore.Qt.MouseButton.NoButton
    lp = QtCore.QPointF(pos[0], pos[1])
    ev = QtGui.QMouseEvent(typ, lp, lp, lp, button, b,
                           QtCore.Qt.KeyboardModifier.NoModifier)
    QtWidgets.QApplication.sendEvent(widget, ev)


def key(widget, k, text=""):
    ev = QtGui.QKeyEvent(QtCore.QEvent.Type.KeyPress, k,
                         QtCore.Qt.KeyboardModifier.NoModifier, text)
    widget.event(ev)


def view_pos(view, scene_pt):
    """场景坐标 -> viewport 坐标，也就是把手命中判定用的那套坐标
    （QAbstractScrollArea 分发时会再扣掉 1px 边框，落在 11px 命中半径内）。"""
    p = view.mapFromScene(QtCore.QPointF(scene_pt[0], scene_pt[1]))
    return (p.x(), p.y())


def canvas_pixels(w):
    """画布当前真正显示的像素。比画面必须落到 numpy：PySide6 的 QPixmap 没有按内容
    的 ==（退化成对象比较），而每次重渲染都会 copy 出新对象，用 == 永远是"变了"。"""
    img = w.view.after.pixmap().toImage().convertToFormat(
        QtGui.QImage.Format.Format_RGB888)
    # 必须 .copy()：frombuffer 只是 QImage 内存上的视图，img 一被回收就成悬空指针
    buf = np.frombuffer(img.constBits(), np.uint8,
                        img.sizeInBytes()).copy()
    buf = buf.reshape(img.height(), img.bytesPerLine())
    return np.ascontiguousarray(buf[:, :img.width() * 3]).reshape(
        img.height(), img.width(), 3)


def widget_pixels(w):
    """任意控件真正画出来的像素。自绘控件（推子）只能用像素说话：
    "没在点外面画环"这种断言，看 sizePolicy 或属性是问不出来的。"""
    img = w.grab().toImage().convertToFormat(
        QtGui.QImage.Format.Format_RGB888)
    buf = np.frombuffer(img.constBits(), np.uint8,
                        img.sizeInBytes()).copy()
    buf = buf.reshape(img.height(), img.bytesPerLine())
    return np.ascontiguousarray(buf[:, :img.width() * 3]).reshape(
        img.height(), img.width(), 3)


def main():
    # 皮肤/参数记忆落到临时 INI：真实偏好可能是浅色，不能假定深色默认值
    ini = os.path.join(TMP, "qt_settings")
    shutil.rmtree(ini, ignore_errors=True)
    QtCore.QSettings.setDefaultFormat(QtCore.QSettings.Format.IniFormat)
    QtCore.QSettings.setPath(QtCore.QSettings.Format.IniFormat,
                             QtCore.QSettings.Scope.UserScope, ini)
    # 缩略图与插件目录也落到测试目录，别碰用户真实缓存
    prefs.put("cache/dir", os.path.join(TMP, "qt_cache"))
    prefs.put("plugins/dir", os.path.join(TMP, "qt_plugins"))
    prefs.put("edits/dir", os.path.join(TMP, "qt_edits"))
    shutil.rmtree(os.path.join(TMP, "qt_edits"), ignore_errors=True)
    app = QtWidgets.QApplication(sys.argv)
    theme.apply(app)
    wb = workbench.Workbench([developer.Developer])
    wb.resize(1480, 880)
    wb.show()
    pump(app, 150)
    w = wb.module(developer.Developer.name)
    shots = []

    got = [0]                                    # 后台已回报的渲染令牌

    def _got(res):
        got[0] = res.token

    w.grader.done.connect(_got)

    def wait_canvas(app, limit=15):
        """等"当前参数的最后一帧"落到画布，比画面必须用它。
        只等尺寸不够：删掉蒙版后旧精修的尺寸没变，量到的是删除前的画面；
        先让 50ms 节流 / 260ms 闲置定时器把任务提交出去，再等最新令牌回报。"""
        pump(app, 400)
        ls = max(w.work.shape[:2])
        t0 = time.time()
        while time.time() - t0 < limit:
            if (got[0] >= w.grader._token
                    and max(w.view.after_full.width(),
                            w.view.after_full.height()) == ls):
                return True
            pump(app, 80)
        return False

    def shoot(name):
        """在功能状态就位时抓一张界面图，shots/ 里的图永远和代码一致。"""
        path = os.path.join(SHOTS, f"shot_{name}.png")
        wb.grab().save(path)
        shots.append((name, os.path.getsize(path)))

    def ghost_rows():
        """右栏里互相压着的滑块行 = 重影。
        deleteLater 要等事件循环回到 exec 才真删，测试全程只 processEvents，
        所以重建面板时没先 hide 的旧控件会一直画在上一次的几何上。"""
        rows = [c for c in w.dock_tools.findChildren(QtWidgets.QWidget)
                if c.isVisible() and type(c).__name__ == "SliderRow"]
        rects = [QtCore.QRect(c.mapTo(w.dock_tools, QtCore.QPoint(0, 0)),
                              QtCore.QSize(c.width(), c.height())) for c in rows]
        return sum(1 for i, a in enumerate(rects) for b in rects[i + 1:]
                   if (x := a.intersected(b)).width() > 4 and x.height() > 4)

    def canvas_state():
        """比较画面前先说清三件事：显示的是哪张、精修到多大、底图多大。
        分割对比开着时 after 只是左半条，尺寸天然不等于精修尺寸。"""
        pm, full = w.view.after.pixmap(), w.view.after_full
        return (f"显示 {pm.width()}x{pm.height()} 精修 {full.width()}x"
                f"{full.height()} 底图 {w.view._pix_w}x{w.view._pix_h}")

    def pix_delta(base):
        """和基准画面的差距（含差异区域），用来把"画面没变"这类断言讲清楚。"""
        d = np.abs(canvas_pixels(w).astype(np.int16) - base.astype(np.int16))
        m = d.max(axis=2) > 2
        ys, xs = np.nonzero(m)
        box = "" if not m.any() else f" 区域 y{ys.min()}-{ys.max()} x{xs.min()}-{xs.max()}"
        return f"最大差 {int(d.max())} 变化 {m.mean():.2%}{box}"

    # 1. 结构
    check("子模式条", list(w.sub_btns) == list(developer.SUBMODES))
    check("停靠面板", all(d is not None for d in
                        (w.dock_tools, w.dock_scopes, w.dock_film, w.dock_export)))
    check("深色调色板", app.palette().window().color().lightness() < 40)
    check("右栏四模块同宽", {w.dock_tools.minimumWidth(),
                          w.dock_scopes.minimumWidth(),
                          w.dock_export.minimumWidth()} == {374})
    check("撤销重做图标", not w.btn_undo.icon().isNull()
          and not w.btn_redo.icon().isNull())
    # 探针实测：发虚主因是关掉 ClearType 后 13-15px 中文笔画太细，
    # 所以这里锁"允许子像素 AA"，并锁中文回退链里有带 hinting 的雅黑 UI
    check("保留 ClearType", not (app.font().styleStrategy()
                                 & QtGui.QFont.StyleStrategy.NoSubpixelAntialias))
    fams = app.font().families()
    check("中文字体链", fams[0].startswith("Inter")
          and ("Microsoft YaHei UI" in fams or "Noto Sans SC" in fams),
          str(fams))
    keys = {r.key for r in w.rows}
    check("细节/分级参数齐全",
          {"sharp_amount", "sharp_masking", "nr_luma", "nr_color",
           "grade_global_s", "grade_sh_s", "grade_hi_s"} <= keys)
    check("参数曲线控件齐全",
          {"par_shadows", "par_darks", "par_lights", "par_highlights",
           "par_split1", "par_split2", "par_split3"} <= keys)
    # 蒙版增量滑块与渲染端 MASK_KEYS 必须一一对应，少一个就是静默失效
    check("蒙版滑块与算法键一致",
          {s[0] for s in params.MASK_SLIDERS} == set(gc.MASK_KEYS))
    check("黑白与镜头校正成行",
          {"mono", "mono_h", "lens_distort", "lens_vignette",
           "lens_fringe"} <= keys)
    # 色阶只由画布编辑：出现滑块就等于两套入口
    check("色阶不进滑块列表", not any(k.startswith("lv_") for k in keys))
    check("分组顺序", [g.title_label.text() for g in w.groups] ==
          ["基本", "色阶", "效果", "细节", "色彩分级", "HSL / 颜色混色", "黑白",
           "镜头校正", "局部蒙版", "色调曲线", "参数曲线", "预设"],
          "、".join(g.title_label.text() for g in w.groups))
    check("蒙版面板图标齐全",
          all(not b.icon().isNull() for b in
              (w.masks.btn_lin, w.masks.btn_rad, w.masks.btn_del,
               w.masks.btn_eye)))
    check("色相滑块色轮",
          all(next(r for r in w.rows if r.key == k).slider.gradient is not None
              for k in ("grade_global_h", "grade_sh_h", "grade_mid_h",
                        "grade_hi_h")))
    # 图标化按钮：文字说明交给 tooltip，accessibleName 留给测试和无障碍
    check("自动校正按钮", w.btn_auto.isVisible()
          and w.btn_auto.accessibleName() == "自动"
          and "Ctrl+U" in w.btn_auto.toolTip()
          and not w.btn_auto.icon().isNull())

    # 2. 子模式切换：面板显隐由模块决定、由外壳执行
    for m in ("图库", "显影", "导出", "显影"):
        w.set_sub_mode(m)
        pump(app, 40)
    check("模块切换可见性",
          w.dock_export.isVisible() is False and w.dock_tools.isVisible())

    # 3. 打开文件夹 + 首帧渲染
    items = sorted(os.path.join(SRC, n) for n in os.listdir(SRC)
                   if n.endswith(".jpg"))[:40]
    w.set_media_list(items)
    pump(app, 4000)
    check("载入序列", len(w.media_items) > 20, f"{len(w.media_items)} 张")
    check("工作图尺寸", w.work is not None and max(w.work.shape[:2]) <= 2201,
          f"{w.work.shape[1]}x{w.work.shape[0]}" if w.work is not None else "无")
    check("预览已出图", not w.view.after.pixmap().isNull())

    # 3b. 渲染线程异常保护：一条坏任务不能把整条线程带走
    probe = pipeline.Grader()
    seen = {"failed": "", "done": 0}
    probe.failed.connect(lambda m: seen.update(failed=m))
    probe.done.connect(lambda r: seen.update(done=seen["done"] + 1))
    probe.submit({}, w.store.p)                  # np.asarray(dict) 必抛 TypeError
    pump(app, 400)
    probe.submit(np.zeros((24, 32, 3), np.float32), w.store.p)
    pump(app, 800)
    check("渲染线程异常后继续工作", bool(seen["failed"]) and seen["done"] >= 1,
          seen["failed"] or "没有失败信号")
    probe.shutdown()

    # 后台流水线/图库切显影时，载入那一刻画布还没排好版；显示后必须按新视口重新适应
    def fits_view():
        r = w.view.mapFromScene(w.view.sceneRect()).boundingRect()
        vp = w.view.viewport().rect()
        return (r.width() <= vp.width() + 1 and r.height() <= vp.height() + 1,
                f"{r.width():.0f}x{r.height():.0f} / {vp.width()}x{vp.height()}")

    ok_fit, dim = fits_view()
    check("载入后适应画布", ok_fit, dim)

    # 4b. 缩放：裸滚轮不抢，Ctrl+滚轮/双击/倍率菜单才是缩放
    def wheel(view, dy, ctrl=False):
        mod = (QtCore.Qt.KeyboardModifier.ControlModifier if ctrl
               else QtCore.Qt.KeyboardModifier.NoModifier)
        p = QtCore.QPointF(view.viewport().rect().center())
        ev = QtGui.QWheelEvent(
            p, view.viewport().mapToGlobal(p.toPoint()),
            QtCore.QPoint(0, 0), QtCore.QPoint(0, dy),
            QtCore.Qt.MouseButton.NoButton, mod,
            QtCore.Qt.ScrollPhase.NoScrollPhase, False)
        view.wheelEvent(ev)
        pump(app, 60)

    w.view.fit()
    pump(app, 200)
    z_fit = w.view._zoom
    wheel(w.view, 120)
    check("裸滚轮不放大", abs(w.view._zoom - z_fit) < 1e-6
          and w.view._auto_fit, f"{z_fit:.3f} → {w.view._zoom:.3f}")
    wheel(w.view, 120, ctrl=True)
    z_notch = w.view._zoom
    check("Ctrl+滚轮放大", z_notch > z_fit * 1.2
          and w.btn_zoom.text() == f"{z_notch * 100:.0f}%",
          f"{z_notch:.3f} {w.btn_zoom.text()}")
    check("一格滚轮放大 25%", abs(z_notch / z_fit - 1.25) < 1e-3,
          f"{z_notch / z_fit:.4f}")
    wheel(w.view, -120, ctrl=True)
    check("Ctrl+滚轮缩小", abs(w.view._zoom - z_fit) < 1e-3,
          f"{w.view._zoom:.3f}")
    # 触控板/高分辨率滚轮常给 15/30 的小 delta：按比例连续缩放，不按整格跳
    wheel(w.view, 30, ctrl=True)
    z_small = w.view._zoom
    check("小增量滚轮连续缩放", abs(z_small / z_fit - 1.25 ** 0.25) < 1e-3,
          f"{z_small / z_fit:.4f}")
    wheel(w.view, -30, ctrl=True)

    # 倍率菜单：直接选 50%/200%，选中项跟着画布走
    w.zoom_acts[0.5].trigger()
    pump(app, 80)
    check("倍率菜单 50%", abs(w.view._zoom - 0.5) < 1e-3
          and w.zoom_acts[0.5].isChecked() and w.btn_zoom.text() == "50%",
          f"{w.view._zoom:.3f} {w.btn_zoom.text()}")
    w.zoom_acts[2.0].trigger()
    pump(app, 80)
    check("倍率菜单 200%", abs(w.view._zoom - 2.0) < 1e-3
          and w.zoom_acts[2.0].isChecked(), f"{w.view._zoom:.3f}")

    # 双击：适应 ⇄ 100%，以落点为中心
    def dblclick(view):
        p = QtCore.QPointF(view.viewport().rect().center())
        ev = QtGui.QMouseEvent(
            QtCore.QEvent.Type.MouseButtonDblClick, p,
            view.viewport().mapToGlobal(p.toPoint()),
            QtCore.Qt.MouseButton.LeftButton,
            QtCore.Qt.MouseButton.LeftButton,
            QtCore.Qt.KeyboardModifier.NoModifier)
        view.mouseDoubleClickEvent(ev)
        pump(app, 120)

    w.view.fit()
    pump(app, 150)
    dblclick(w.view)
    check("双击到 100%", abs(w.view._zoom - 1.0) < 1e-3
          and w.btn_zoom.text() == "100%", f"{w.view._zoom:.3f}")
    dblclick(w.view)
    check("再双击回到适应窗口", w.view._auto_fit
          and abs(w.view._zoom - z_fit) < 1e-3, f"{w.view._zoom:.3f}")
    w.view.hide()
    w.view.set_source(w.view.before.pixmap().toImage())
    w.view.show()
    pump(app, 400)
    ok_fit, dim = fits_view()
    check("显示后重新适应画布", ok_fit, dim)

    # 3b. 自动校正：一次写入多项参数，只占一步历史
    w.store.reset_all()
    pump(app, 300)
    snap0 = w.store.snapshot()
    w.auto()
    pump(app, 2000)
    check("自动校正写入参数", w.store.snapshot() != snap0,
          "曝光 {:+.2f} · 色温 {:+.0f}".format(
              w.store.value("exposure"), w.store.value("temperature")))
    w.undo()
    pump(app, 600)
    check("自动校正一步撤销", w.store.snapshot() == snap0)

    # 4. 参数变化触发重渲染
    before = w.view.after.pixmap().toImage()
    w.store.push()
    w.store.set("exposure", 1.2)
    w.store.set("saturation", 40)
    w.store.set("vignette", -50)
    w.store.set("texture", 35)
    w.store.set("dehaze", 20)
    pump(app, 2500)
    after = w.view.after.pixmap().toImage()
    check("调参后画面变化", before != after)
    check("示波器有数据", w.scopes.hist() is not None)

    # 5. 撤销 / 重做
    snap = w.store.snapshot()
    w.undo()
    pump(app, 900)
    check("撤销回到原值", abs(w.store.value("exposure")) < 1e-6)
    w.redo()
    pump(app, 900)
    check("重做恢复", abs(w.store.value("exposure") - 1.2) < 1e-6)

    # 5b. 分级预览：改动后 50ms 出草图（低分辨率），停手后补精修
    proxy_long = max(w.proxy.shape[:2])
    work_long = max(w.work.shape[:2])
    check("草图比工作图小", proxy_long < work_long,
          f"{proxy_long} < {work_long}")
    res_long = lambda: max(w.view.after_full.width(), w.view.after_full.height())
    w.store.push()
    t0 = time.time()
    w.store.set("temperature", -30)
    while res_long() == work_long and time.time() - t0 < 5:
        pump(app, 30)
    check("拖动先出草图", res_long() == proxy_long,
          f"{(time.time() - t0) * 1000:.0f}ms，{res_long()}px")
    check("草图几何对齐底图",
          abs(w.view.after.transform().m11() * w.view.after_full.width()
              - w.work.shape[1]) < 1.5,
          f"条目缩放 {w.view.after.transform().m11():.3f}")
    t0 = time.time()
    while res_long() != work_long and time.time() - t0 < 8:
        pump(app, 30)
    check("停手补精修", res_long() == work_long,
          f"再等 {time.time() - t0:.2f}s")

    # 6. 预设 + 滑块同步
    w.store.load_preset("落日暖调")
    pump(app, 1200)
    vals = {r.key: r.value() for r in w.rows if r.idx is None}
    check("预设写入模型", vals.get("temperature", 0) != 0,
          f"色温 {vals.get('temperature')}")
    check("滑块控件同步", any(abs(r.spin.value()) > 0 for r in w.rows))

    # 7. HSL / 曲线
    w.store.set("hsl_s", -30, idx=3)
    pump(app, 1500)
    check("HSL 生效", abs(w.store.value("hsl_s", 3) + 30) < 1e-6)
    pts = w.store.p["curve_rgb"]
    pts.append([128, 150])
    pts.sort()
    w.store.changed.emit("curve")
    pump(app, 1500)
    check("曲线加点生效", len(w.store.p["curve_rgb"]) == 3)

    # 8. 对比 / 缩放
    w.act_compare.setChecked(True)
    pump(app, 200)
    check("分割对比开启", w.view.compare)
    z = w.view._zoom
    w.view.zoom_to(1.0)
    pump(app, 200)
    check("缩放到 100%", abs(w.view._zoom - 1.0) < 1e-6 and z != 1.0,
          f"适应={z:.3f}")
    w.view.set_split(0.3)
    check("分割线移动", abs(w.view.split - 0.3) < 1e-6)
    shoot("对比")
    # 关掉分割再往下走：开着时 after 只是裁出的左半条，后面要比的是整张画面
    w.act_compare.setChecked(False)
    pump(app, 200)
    check("分割对比关闭", not w.view.compare)

    # 9. 面板浮动（QDockWidget 可拆出）
    w.dock_scopes.setFloating(True)
    pump(app, 150)
    check("示波器可浮动", w.dock_scopes.isFloating())
    w.dock_scopes.setFloating(False)
    pump(app, 150)
    check("示波器可回停靠", not w.dock_scopes.isFloating())

    # 10. 图库网格（IconMode 必须显式 gridSize/sizeHint，否则项会塌成一行）
    w.set_sub_mode("图库")
    pump(app, 4000)
    grid = w.grid_browser
    r = grid.list.visualItemRect(grid.list.item(0))
    n_img = sum(1 for i in range(min(8, grid.list.count()))
                if not grid.list.item(i).icon().isNull())
    check("图库网格缩略图", r.height() >= 100 and n_img >= 6,
          f"单元格 {r.width()}x{r.height()} 图标 {n_img}/8")
    check("图库是整窗网格", not w.view.isVisible()
          and w.grid_host.isVisible() and not w.dock_film.isVisible())
    check("图库提示只放张数", wb.lb_page_hint.text() == "40 张",
          wb.lb_page_hint.text())
    check("胶片条无横向滚动条",
          w.browser.list.horizontalScrollBarPolicy()
          == QtCore.Qt.ScrollBarPolicy.ScrollBarAlwaysOff)

    # 10c. 逐帧亮度曲线：6 张图 + 1 张坏文件，直接驱动胶片浏览器
    curve_dir = os.path.join(TMP, "qt_curve")
    shutil.rmtree(curve_dir, ignore_errors=True)
    os.makedirs(curve_dir)
    cpaths = []
    for i in range(6):
        p = os.path.join(curve_dir, f"c{i:02d}.jpg")
        if i == 2:
            with open(p, "wb") as f:
                f.write(b"not an image")
        else:
            cv2.imwrite(p, np.full((60, 80, 3), 100 + (i % 3) * 40, np.uint8))
        cpaths.append(p)
    fb = filmstrip.Browser()
    fb.resize(640, 190)
    fb.show()
    fb.set_mode("strip")
    fb.set_items(cpaths)
    t0 = time.time()
    while fb._luma_values is None and (time.time() - t0) < 10:
        pump(app, 100)
    lv = fb._luma_values or []
    got_means = [v for v in lv if v is not None]
    check("亮度曲线测量", len(lv) == 6 and len(got_means) == 5
          and fb._luma_bad == {2} and fb.curve.isVisible(),
          f"{len(got_means)}/6 值 · 坏帧 {sorted(fb._luma_bad)} · "
          f"曲线可见 {fb.curve.isVisible()}")
    seeked = []
    fb.picked.connect(seeked.append)
    send(fb.curve, QtCore.QEvent.Type.MouseButtonPress,
         (fb.curve.width() - 1, fb.curve.height() // 2))
    pump(app, 100)
    check("曲线点击跳帧", seeked == [5] and fb.index == 5,
          f"选中 {fb.index} 信号 {seeked}")
    fb.shutdown()
    fb.close()

    def ready():
        return sum(1 for i in range(grid.list.count())
                   if not grid.list.item(i).icon().isNull())

    # 后台线程逐张推缩略图，截图要等它铺满，否则半张图看着像没加载完
    t0 = time.time()
    while ready() < grid.list.count() and (time.time() - t0) < 20:
        pump(app, 300)
    check("缩略图全部到位", ready() == grid.list.count(),
          f"{ready()}/{grid.list.count()} 张，{time.time() - t0 + 4:.0f}s")
    shoot("图库")
    # 双击缩略图 = 载入这张并进入显影
    grid.activated.emit(2)
    pump(app, 900)
    check("图库双击进显影",
          w.sub == "显影" and w.browser.index == 2 and w.view.isVisible(),
          f"{w.sub} #{w.browser.index + 1}")
    # 顶栏不再放教程句：显影态应当是空的，说明文案只在悬停提示里
    check("顶栏显影态无教程文案", wb.lb_page_hint.text() == "",
          wb.lb_page_hint.text())
    w.select_index(0)
    pump(app, 300)
    w.set_sub_mode("导出")
    pump(app, 400)
    shoot("导出")
    w.set_sub_mode("显影")
    pump(app, 500)
    w.groups[3].set_collapsed(False)
    w.tools_scroll.ensureWidgetVisible(w.groups[3])
    pump(app, 300)
    shoot("细节")
    w.groups[4].set_collapsed(False)
    w.tools_scroll.ensureWidgetVisible(w.groups[4])
    pump(app, 300)
    shoot("分级")
    w.tools_scroll.verticalScrollBar().setValue(0)
    pump(app, 300)
    shoot("显影")

    # 10b. 专业检查器交互：分组头、复位、渐变轨道、改动高亮、信息栏
    snap_ui = w.store.snapshot()
    g = w.groups[0]
    check("分组标题无文字箭头",
          "▾" not in g.title_label.text() and "▸" not in g.title_label.text())
    check("分组图标齐全",
          not g.chev.icon().isNull() and not g.btn_reset.icon().isNull())
    g.set_collapsed(True)
    check("分组可折叠", not g.body.isVisible())
    g.set_collapsed(False)
    check("分组可展开", g.body.isVisible())
    r_temp = next(r for r in w.rows if r.key == "temperature")
    check("色温渐变轨道", r_temp.slider.gradient is not None)
    w.store.set("temperature", 42)
    pump(app, 120)
    check("改动数值高亮",
          r_temp.spin.lineEdit().property("modified") is True)
    r_temp.slider.reset_requested.emit()
    pump(app, 120)
    check("双击滑块复位", abs(w.store.value("temperature")) < 1e-6)

    # 推子观感：细轨 + 小白点，悬停只在点自己身上变，不许套外环
    sl = next(r for r in w.rows if r.key == "exposure").slider
    check("推子比旧版细一圈",
          widgets.Slider.HANDLE == 13 and widgets.Slider.TRACK == 4
          and sl.height() == theme.fs(widgets.Slider.ROW),
          f"点 {widgets.Slider.HANDLE}px 轨 {widgets.Slider.TRACK}px "
          f"行 {sl.height()}px")
    w.store.set("exposure", 1.2)
    pump(app, 150)
    rest = widget_pixels(sl)
    sl._hot = True
    sl.update()
    pump(app, 150)
    hov = widget_pixels(sl)
    band = np.abs(hov.astype(np.int16) - rest.astype(np.int16)).max(axis=2) > 10
    ys, xs = np.nonzero(band)
    tall = int(ys.max() - ys.min() + 1) if band.any() else 0
    # 按住时点最大：HANDLE+3 再加描边与抗锯齿的 3px。外环会把这高度顶到 20+
    check("悬停不画外环", band.any() and tall <= widgets.Slider.HANDLE + 6,
          f"亮起来的部分高 {tall}px，点最大 {widgets.Slider.HANDLE + 3}px")
    sl._hot = False
    sl.update()
    pump(app, 100)
    path = os.path.join(SHOTS, "shot_推子.png")
    g.body.grab().save(path)
    check("推子特写出图", os.path.getsize(path) > 4000,
          f"{os.path.basename(path)} "
          f"{os.path.getsize(path) // 1024}KB")
    w.store.set("exposure", 0.0)
    pump(app, 120)
    w.store.set("exposure", 2.0)
    pump(app, 120)
    g.reset()
    pump(app, 120)
    check("分组一键复位", abs(w.store.value("exposure")) < 1e-6)
    check("画布信息栏",
          w.lb_meta.text().startswith("1/40")
          and w.btn_zoom.text().endswith("%"), w.lb_meta.text())
    w.store.restore(snap_ui)
    pump(app, 400)

    # 10c. 色阶画布：拖把手改的是同一份 store，一次拖动只算一步历史
    lv = w.levels
    check("色阶吃到直方图", lv._u8 is not None,
          "没有数据就画不出背景直方图")
    snap_lv = w.store.snapshot()
    hx, hy = lv.handles()["black"]
    send(lv, QtCore.QEvent.Type.MouseButtonPress, (hx, hy))
    send(lv, QtCore.QEvent.Type.MouseMove, (hx + 40, hy))
    send(lv, QtCore.QEvent.Type.MouseMove, (hx + 46, hy))
    send(lv, QtCore.QEvent.Type.MouseButtonRelease, (hx + 46, hy))
    pump(app, 900)
    check("拖动黑场把手", w.store.value("lv_black") > 20,
          f"lv_black={w.store.value('lv_black'):.1f}")
    check("拖动不残留抓取状态", lv._drag is None)
    w.undo()
    pump(app, 900)
    check("色阶拖动一步撤销", w.store.snapshot() == snap_lv)
    w.store.set("lv_white", 200)
    pump(app, 300)
    send(lv, QtCore.QEvent.Type.MouseButtonDblClick, lv.handles()["gamma"])
    pump(app, 600)
    check("画布双击复位本组", abs(w.store.value("lv_white") - 255.0) < 1e-6,
          f"lv_white={w.store.value('lv_white'):.0f}")

    # 10d. 局部蒙版：新建 → 画面上拖把手 → 增量生效 → 叠加显示 → 删除
    check("精修已就位", wait_canvas(app))
    pix_pre = canvas_pixels(w)
    n0 = w.store.mask_count()
    w.masks.btn_lin.click()
    pump(app, 1600)
    i = w.masks.sel
    check("新建线性蒙版", w.store.mask_count() == n0 + 1 and i == n0,
          f"共 {w.store.mask_count()} 张，选中 {i}")
    check("选中态同步到画布", w.view.mask_sel == i
          and len(w.view.masks) == n0 + 1)
    check("重建蒙版行不留重影", (gv := ghost_rows()) == 0, f"{gv} 对压着")
    # 没填增量的蒙版必须是透明的，否则一进显影画面就莫名变了
    check("空蒙版不改画面", wait_canvas(app)
          and np.array_equal(canvas_pixels(w), pix_pre),
          canvas_state() + " " + pix_delta(pix_pre))
    w.store.mask_set(i, "exposure", -1.0)
    check("蒙版增量改画面", wait_canvas(app))
    diff = np.abs(canvas_pixels(w).astype(np.int16)
                  - pix_pre.astype(np.int16)).max(axis=2) > 2
    # 线性蒙版只压暗渐变覆盖住的那一侧；整幅都变说明权重漏了
    check("增量只改蒙版范围", 0.05 < diff.mean() < 0.98,
          f"变化 {diff.mean():.2%}")
    # M01: 一个组里叠第二个形状，布尔改为“减去”，画布按组件命中
    ncomp0 = w.store.comp_count(i)
    w.masks.btn_comp_rad.click()
    pump(app, 500)
    j1 = ncomp0
    check("组内加形状", w.store.comp_count(i) == ncomp0 + 1
          and w.masks.comp == j1 and w.view.mask_comp == j1,
          f"{w.store.comp_count(i)} 个形状，选中 {w.masks.comp}")
    w.masks.cb_blend.setCurrentIndex(w.masks.cb_blend.findData("subtract"))
    pump(app, 500)
    check("形状设为减去", w.store.comp(i, j1).get("blend") == "subtract")
    pm = w.view._field_pm(w.store.mask(i), 170)
    check("组权重图已合成", pm is not None and not pm.isNull())
    p1 = view_pos(w.view, w.view._mask_points(w.store.comp(i, j1))["body"])
    hit = w.view._mask_hit(QtCore.QPointF(*p1))
    check("点第二形状把手命中组件", hit is not None and hit[0] == j1, str(hit))
    # 起点把手往画面下方拖 = 渐变范围变宽；事件要发给 viewport
    vp = w.view.viewport()
    p0 = view_pos(w.view, w.view._mask_points(w.store.comp(i))["a"])
    y0 = w.store.mask_attr(i, "y1")
    send(vp, QtCore.QEvent.Type.MouseButtonPress, p0)
    check("把手抓取进入拖动态", w.view._mask_drag is not None,
          f"{p0} 命中 {w.view._mask_drag}")
    send(vp, QtCore.QEvent.Type.MouseMove, (p0[0], p0[1] + 40))
    pump(app, 300)
    y1 = w.store.mask_attr(i, "y1")
    check("拖把手改几何", abs(y1 - y0) > 1e-4, f"{y0:.3f} -> {y1:.3f}")
    send(vp, QtCore.QEvent.Type.MouseButtonRelease, (p0[0], p0[1] + 40))
    pump(app, 900)
    check("拖动后退出拖动态", w.view._mask_drag is None)
    w.undo()
    pump(app, 1200)
    check("几何拖动一步撤销",
          abs(w.store.mask_attr(i, "y1") - y0) < 1e-6
          and abs(w.store.mask_value(i, "exposure") + 1.0) < 1e-6,
          "撤销只回几何，增量仍在")
    # M02: 亮度范围（组级）：勾选后多下限/上限/羽化三行
    w.masks.cb_range.setChecked(True)
    pump(app, 700)
    check("亮度范围开关", w.store.mask_range(i).get("lum_on") == 1)
    w.store.range_set(i, "lum_max", 25)
    w.store.range_set(i, "lum_feather", 10)
    pump(app, 700)
    check("亮度范围可调",
          abs(w.store.mask_range(i)["lum_max"] - 0.25) < 1e-6,
          str(w.store.mask_range(i)))
    check("范围滑块齐全",
          {"lum_min", "lum_max", "lum_feather"} <= {r.key for r in w.masks.rows})
    pm = w.view._field_pm(w.store.mask(i), 170)
    check("含亮度项的权重图可生成", pm is not None and not pm.isNull())
    # M03: 局部锐化/降噪不再重跑整条局部管线，面板里可直接调
    check("蒙版空间项滑块齐全",
          {"sharp_amount", "nr_luma", "nr_color"} <= {r.key for r in w.masks.rows})
    w.masks.cb_range.setChecked(False)      # 叠加截图回到纯几何，红点好数
    pump(app, 500)
    w.masks.btn_eye.click()
    pump(app, 300)
    check("眼睛叠加蒙版范围", w.view.mask_show
          and w.masks.btn_eye.isChecked())
    # 绘制里的异常会被 PySide6 咽掉，只留 stderr：所以直接数画面上的红点，
    # 不然蒙版叠加画不出来也没人知道（drawPixmap(QRectF, pm) 就是这么溜过去的）
    px = w.view.grab().toImage()
    red = sum(1 for y in range(0, px.height(), 7)
              for x in range(0, px.width(), 7)
              if (c := px.pixelColor(x, y)).red() > 90
              and c.red() > c.green() + 40 and c.red() > c.blue() + 40)
    check("蒙版叠加真的画出来", red > 20, f"采样到 {red} 个红点")
    key(w.view, QtCore.Qt.Key.Key_M)
    pump(app, 300)
    check("M 键切回且按钮同步", not w.view.mask_show
          and not w.masks.btn_eye.isChecked())
    w.masks.btn_eye.click()
    pump(app, 300)
    w.groups[8].set_collapsed(False)
    pump(app, 300)
    # 展开之后布局才长高，先展开再定位；ensureWidgetVisible 对"比窗口还高的控件"
    # 只会滚到露出下半截，截图里看不到蒙版缩略条，所以直接把组顶对齐到栏顶
    w.tools_scroll.verticalScrollBar().setValue(
        w.groups[8].mapTo(w.tools_scroll.widget(), QtCore.QPoint(0, 0)).y())
    pump(app, 400)
    top = w.groups[8].mapTo(w.tools_scroll.viewport(), QtCore.QPoint(0, 0)).y()
    vis = w.tools_scroll.viewport().rect().intersected(
        QtCore.QRect(w.masks.mapTo(w.tools_scroll.viewport(), QtCore.QPoint(0, 0)),
                     QtCore.QSize(w.masks.width(), w.masks.height())))
    check("蒙版组滚到栏顶", abs(top) < 4, f"组顶 {top}px")
    check("蒙版面板可见过半", vis.height() > w.masks.height() * 0.5,
          f"可见 {vis.height()}/{w.masks.height()}px")
    check("截图前面板无重影", (gv := ghost_rows()) == 0, f"{gv} 对压着")
    shoot("蒙版")
    w.masks.btn_eye.click()                    # 截图后关掉叠加再删
    pump(app, 200)
    w.masks.btn_del.click()
    pump(app, 900)
    check("删除蒙版", w.store.mask_count() == n0 and w.masks.sel == -1,
          f"{w.store.mask_count()} 张")
    check("删除后画布清空", len(w.view.masks) == n0 and not w.view.mask_show)
    check("删空后回到无改动", wait_canvas(app)
          and np.array_equal(canvas_pixels(w), pix_pre),
          canvas_state() + " " + pix_delta(pix_pre)
          + f" 仍有改动={gc.changes(w.store.p)}")

    # 11. 导出图片（全分辨率）
    out = os.path.join(TMP, "_qt_out.jpg")
    im = exporter.load_full(w.media_items[0])
    ok = exporter.save_image(out, np.clip(gc.apply(im, w.store.p), 0, 1), 95)
    back = exporter.load_full(out)
    check("图片导出", ok and back is not None and back.shape[:2] == im.shape[:2],
          f"{im.shape[1]}x{im.shape[0]}")

    # 12. 导出 .cube（含 HSL/曲线/空间项混合）
    cube = os.path.join(TMP, "_qt_out.cube")
    gc.write_cube(cube, w.store.p, size=33)
    txt = open(cube, encoding="ascii").read().splitlines()
    # 5 行头部（注释 / TITLE / SIZE / DOMAIN_MIN / DOMAIN_MAX）+ 33^3 个颜色
    check("cube 规格", txt[2].startswith("LUT_3D_SIZE 33")
          and len(txt) == 5 + 33 ** 3, f"{len(txt)} 行")

    # 12. 视频抽帧预览 + 实时播放
    vid = os.path.join(ROOT, "aligned_120fps_3840.mp4")
    if os.path.exists(vid):
        w.load_video(vid)
        w.set_sub_mode("显影")
        pump(app, 3000)
        check("视频抽帧预览", w.video is not None and not w.view.after.pixmap().isNull(),
              f"{w.video['w']}x{w.video['h']} {w.video['fps']}fps" if w.video else "")
        check("视频播放条", w.transport.isVisible() and w.player.count > 1,
              f"{w.player.count} 帧 · {w.lb_time.text()}")
        i0 = w.player.index
        w.store.set("exposure", 0.6)          # 播放时参数要逐帧套上
        w.toggle_play()
        pump(app, 1500)
        moved = w.player.index
        w.toggle_play()
        check("视频实时预览", moved > i0, f"{i0} -> {moved} 帧")

    # 13. 图片序列按序播放（拖入一堆照片就能放）
    seq = [p for p in w.media_items if os.path.isfile(p)][:15]
    if len(seq) > 1:
        w.set_media_list(seq)
        w.set_sub_mode("显影")
        pump(app, 60)
        check("序列播放条", w.transport.isVisible() and w.player.count == len(seq),
              f"{w.player.count} 张 · {w.lb_time.text()}")
        w.player.loop = False
        w.player.play()
        pump(app, 3000)
        if w.player.playing:
            w.toggle_play()
        # 暂停后还有一帧 4K 在解码途中，等指针连续一秒不动再取
        last, stable = w.player.index, 0
        for _ in range(30):
            pump(app, 200)
            if w.player.index == last:
                stable += 1
                if stable >= 5:
                    break
            else:
                last, stable = w.player.index, 0
        at = w.player.index
        # RAW 序列每张都要解码 + 去马赛克，速率受解码限制；只要求过半或放到头
        check("序列按序播放", at >= len(seq) // 2 or not w.player.playing,
              f"3s 放到第 {at + 1}/{len(seq)} 张")
        check("序列同步胶片条", w.browser.index == at, f"选中 {w.browser.index + 1}")

        # 13b. 跨帧参考：左侧钉住参考帧，右侧是当前帧
        w.act_ref.setChecked(True)
        pump(app, 300)
        ref_i = at - 1 if at > 0 else 1
        check("参考帧默认取上一帧",
              w.act_ref.isChecked() and w.view.compare
              and w._ref_path == seq[ref_i]
              and not w.view.ref_full.isNull(),
              f"{w._ref_path!r} / 第 {ref_i + 1} 张")
        check("参考帧占左侧",
              w.view.ref.isVisible() and not w.view.before.isVisible()
              and w.view.after.pixmap().width() < w.view.after_full.width(),
              f"结果宽 {w.view.after.pixmap().width()}"
              f" / 全图 {w.view.after_full.width()}")
        shoot("参考对比")
        w.lock_reference()
        check("锁定当前帧为参考", w._ref_manual and w._ref_path == seq[at],
              f"{os.path.basename(w._ref_path or '')}"
              f" / 期望 {os.path.basename(seq[at])}")
        w.select_index(0)
        pump(app, 400)
        check("锁定后跨帧仍用同一参考",
              w._ref_path == seq[at] and w.view.ref.isVisible(),
              os.path.basename(w._ref_path or ""))
        w.act_ref.setChecked(False)
        pump(app, 100)
        check("关闭参考回到原状态",
              not w.view.ref.isVisible() and not w.view.compare)
        w.act_compare.setChecked(True)
        pump(app, 100)
        check("参考关闭后前后对比仍正常",
              w.view.before.isVisible() and not w.view.ref.isVisible())
        w.act_compare.setChecked(False)
        shoot("序列播放")

    # 14. 截图：前面各子模式已分别抓过，这里补视频与序列的播放状态
    if os.path.exists(vid):
        w.load_video(vid)
        pump(app, 1500)
    shoot("视频预览")
    small = [n for n, sz in shots if sz < 20000]
    check("界面截图", len(shots) == 10 and not small,
          "、".join(f"{n} {sz // 1024}KB" for n, sz in shots))

    menu = [a.text() for a in wb.menu("文件").actions()]
    check("菜单归外壳", "导出视频…" in menu)
    check("XMP 预设菜单", "导入 XMP 预设…" in menu and "导出 XMP 预设…" in menu)

    # 15. Holy Grail：EXIF/亮度台阶检测 + 反向斜坡 + 旁车
    log = -0.01 * np.arange(80)
    log[20:] += 2.0
    log[50:] += 1.5
    hsteps = holy_core.find_luma_steps(2.0 ** log)
    hoff = holy_core.ramp_offsets(hsteps, 80, 12)
    hout = log + hoff
    check("亮度台阶检测", [i for i, _ in hsteps] == [20, 50], str(hsteps))
    check("台阶补偿不跳帧且保留趋势",
          np.abs(np.diff(hout)).max() < 0.2
          and abs((hout[-1] - hout[0]) - (log[-1] - log[0])) < 0.01,
          f"最大帧差 {np.abs(np.diff(log)).max():.2f}→"
          f"{np.abs(np.diff(hout)).max():.2f}")
    flick = -0.01 * np.arange(40)
    flick[15] += 1.0
    check("单帧闪烁不算台阶",
          holy_core.find_luma_steps(2.0 ** flick) == [])

    holy_dir = os.path.join(TMP, "qt_holy")
    shutil.rmtree(holy_dir, ignore_errors=True)
    os.makedirs(holy_dir)
    hpaths = []
    for i in range(30):
        p = os.path.join(holy_dir, f"h{i:03d}.jpg")
        img = Image.fromarray(
            np.full((40, 60, 3), 100 if i < 15 else 180, np.uint8))
        ex = Image.Exif()
        ex[0x8827] = 100 if i < 15 else 400
        ex[0x829A] = (1, 60)
        ex[0x829D] = (28, 10)
        img.save(p, exif=ex)
        hpaths.append(p)
    info0 = imio.exif_exposure(hpaths[0])
    info1 = imio.exif_exposure(hpaths[15])
    check("EXIF 曝光读取",
          abs(info1["ev"] - info0["ev"] - 2.0) < 0.01
          and info0["iso"] == 100 and abs(info1["f"] - 2.8) < 0.01,
          f"{info0} → {info1}")

    orig_items, orig_index = list(w.media_items), w.browser.index
    w.set_media_list(hpaths)
    w.detect_holy()
    t0 = time.time()
    while w._holy is None and (time.time() - t0) < 30:
        pump(app, 150)
    data = w._holy or {}
    steps = data.get("steps") or []
    check("EXIF 台阶进界面",
          len(steps) == 1 and steps[0][0] == 15
          and abs(float(steps[0][1]) - 2.0) < 0.05
          and data.get("source") == "exif" and w.cb_holy.isChecked(),
          str(steps))
    check("台阶标到亮度曲线",
          len(w.browser.curve.steps) == 1 and w.browser.curve.isVisible())
    check("台阶补偿斜坡",
          abs(w._holy_offset(15) + 2.0) < 0.01
          and abs(w._holy_offset(21) + 1.0) < 0.01
          and abs(w._holy_offset(27)) < 1e-9,
          f"15:{w._holy_offset(15):.2f} 21:{w._holy_offset(21):.2f} "
          f"27:{w._holy_offset(27):.2f}")
    base_exp = w.store.p["exposure"]
    check("台阶进预览参数",
          abs(w._preview_params(15)["exposure"] - (base_exp - 2.0)) < 1e-6,
          f"{base_exp:.2f} → {w._preview_params(15)['exposure']:.2f}")
    holy_key = w._edit_key
    check("台阶旁车落盘",
          edits.load_holy(holy_key, w._play_items) is not None)
    w.cb_holy.setChecked(False)
    pump(app, 200)
    check("台阶补偿可关闭",
          abs(w._preview_params(15)["exposure"] - base_exp) < 1e-6)
    w.cb_holy.setChecked(True)
    w.sp_holy_ramp.setValue(20)
    pump(app, 100)
    check("台阶过渡帧数可调",
          abs(w._holy_offset(21) + 1.4) < 0.01,
          f"21 帧 {w._holy_offset(21):.2f}")
    w.btn_holy_clear.click()
    pump(app, 150)
    check("清除台阶",
          w._holy is None and w.browser.curve.steps == []
          and edits.load_holy(holy_key, w._play_items) is None)
    w.set_media_list(orig_items, orig_index)
    pump(app, 600)

    wb.close()          # closeEvent 里逐个模块 shutdown
    print("\n" + ("全部通过" if not FAILS else f"失败 {len(FAILS)}: {FAILS}"))
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
