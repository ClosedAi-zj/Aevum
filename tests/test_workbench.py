# -*- coding: utf-8 -*-
"""工作台外壳回归：工具注册、自由切换、面板显隐、素材流水线、全局进度与取消。

没有左侧模块栏：顶部标签切换工具，六个工具完全独立，导入素材只自动填内容。
外壳只有一条进度条和一个取消按钮；合成写完的视频要自动进调色台。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_workbench.py
"""
import os
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "acceptance"))
TMP = os.path.join(HERE, "_tmp")
SHOTS = os.path.join(ROOT, "shots")
os.makedirs(TMP, exist_ok=True)

import cv2                                # noqa: E402
import numpy as np                        # noqa: E402
from PySide6 import QtCore, QtGui, QtWidgets     # noqa: E402

from reference import deflicker as flicker    # noqa: E402
from reference import ff as ffmod             # noqa: E402
from reference import grade as gc             # noqa: E402
from reference import plugins as pluginmod    # noqa: E402
from reference import sequence as seq         # noqa: E402
from reference.ff import find_ffmpeg, probe_video   # noqa: E402
from reference.gui import edits               # noqa: E402
from reference.gui import job as jobs         # noqa: E402
from reference.gui import prefs, theme, workbench   # noqa: E402
from reference.gui import aligner, assemble, cropper   # noqa: E402
from reference.gui import playback                     # noqa: E402
from reference.gui.grader import deflicker as flicker_ui   # noqa: E402
from reference.gui.grader import developer, exporter, filmstrip   # noqa: E402

# 用入口真正注册的那份模块表：测试跑的就是用户双击出来的那个工作台
from apps.workbench import MODULES        # noqa: E402

SERIES = os.path.join(TMP, "wb_series")
SRC_BIG = os.path.join(ROOT, "aligned_lr3")
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


def wait_job(app, wb, timeout=180):
    """等后台任务跑完（信号要主线程转圈才收得到）。"""
    t0 = time.time()
    while wb._job is not None and wb._job.isRunning() and time.time() - t0 < timeout:
        pump(app, 60)
    pump(app, 200)
    return wb._job is None or not wb._job.isRunning()


def wait_import(app, wb, timeout=60):
    """等后台导入收工：素材是发布完才换的，断言前得先在事件圈里绕到它。"""
    t0 = time.time()
    while wb._import is not None and wb._import.isRunning() and time.time() - t0 < timeout:
        pump(app, 40)
    pump(app, 150)
    return wb._import is None


def make_series(n=14, d=SERIES):
    os.makedirs(d, exist_ok=True)
    for f in os.listdir(d):
        os.remove(os.path.join(d, f))
    for i in range(n):
        a = np.zeros((300, 400, 3), np.uint8)
        a[:, :, :] = (30 + i * 8, 90, 200 - i * 8)
        cv2.circle(a, (200 + i * 10, 150), 40, (255, 255, 255), -1)
        cv2.imwrite(os.path.join(d, f"wb_{i:04d}.jpg"), a)
    return d


def visible_docks(w):
    return sorted(d.windowTitle() for d in w.findChildren(QtWidgets.QDockWidget)
                  if d.isVisible())


def shoot(wb, name):
    """外壳截图：shots/ 里的工作台图由回归顺手生成，不留手工拍的老图。"""
    path = os.path.join(SHOTS, f"shot_{name}.png")
    wb.grab().save(path)
    return path, os.path.getsize(path)


def test_selftest(series):
    """打包版自检真跑一遍（子进程）：那条路只在 exe 里走，最容易烂在没人点。

    它引用过已经改名的控件字典（`_sum_vals`），异常被窗口版 exe 吞掉，
    直到那次重打包才暴露。所以这条必须在 GUI 全部关掉之后单独起一个进程，
    结果只认 `%TEMP%\\lapse_selftest.txt`——那正是打包版唯一的输出渠道。
    """
    report = os.path.join(tempfile.gettempdir(), "lapse_selftest.txt")
    if os.path.exists(report):
        os.remove(report)
    proc = subprocess.run([sys.executable,
                           os.path.join(ROOT, "apps", "workbench.py"),
                           "--selftest", series],
                          capture_output=True, text=True, encoding="utf-8",
                          timeout=600)
    txt = (open(report, encoding="utf-8").read()
           if os.path.exists(report) else "（自检没写报告）")
    bad = [l for l in txt.splitlines() if l.startswith("[FAIL]")]
    check("自检：退出码与报告一致", (proc.returncode == 0) == ("结果：通过" in txt),
          f"rc={proc.returncode} " + " | ".join(bad)[:200])
    check("自检：每一行都过", not bad, str(bad)[:260])
    check("自检：六个工具都在", "调色台 / 对齐 / 合成 / 裁剪 / 配乐 / 运动" in txt,
          " | ".join(l for l in txt.splitlines() if "六个工具" in l)[:120])
    check("自检：预检真的算出位移且没写文件",
          any("预检出结果" in l for l in txt.splitlines())
          and not any("自检异常" in l for l in txt.splitlines()),
          " | ".join(l for l in txt.splitlines() if "预检" in l)[:200])


def test_meter():
    m = jobs.Meter("frame", 20)
    f, d = m.feed("frame=         5")
    check("Meter 帧进度", f == 0.25 and "帧 5/20" in d, f"{f} {d}")
    f, _ = m.feed("progress=end")
    check("Meter 收尾", f == 1.0, str(f))
    m = jobs.Meter("time", 10.0)
    f, _ = m.feed("out_time_us=2500000")
    check("Meter 时长进度", abs(f - 0.25) < 1e-6, str(f))
    # speed / total_size 不是进度行：先记下，等于下一条进度行的附加信息
    check("Meter 速度行不报进度", m.feed("speed=3.2x")[0] is None)
    check("Meter 体积行不报进度", m.feed("total_size=1048576")[0] is None)
    f, d = m.feed("out_time_us=3000000")
    check("Meter 速度体积进详情", "3.2x" in d and "MB" in d, d)
    f, _ = m.feed("pipe:0-> Invalid data")
    check("Meter 非进度行返回 None", f is None)


def test_shell(app):
    wb = workbench.Workbench(MODULES)
    wb.resize(1400, 860)
    wb.show()
    pump(app, 200)
    asm = wb.module(assemble.Assemble.name)
    dev = wb.module(developer.Developer.name)
    alg = wb.module(aligner.Aligner.name)

    check("顶部标签顺序",
          list(wb.mod_btns) == ["调色台", "对齐", "合成", "裁剪",
                                          "配乐", "运动"],
          str(list(wb.mod_btns)))
    check("已撤掉左侧模块栏", not getattr(wb, "rail", None))
    check("页面数", wb.pages.count() == len(MODULES), str(wb.pages.count()))
    check("全局只有一条进度", len(wb.findChildren(QtWidgets.QProgressBar)) == 1,
          str(len(wb.findChildren(QtWidgets.QProgressBar))))
    acts = [a.text() for a in wb.menu("文件").actions()]
    check("模块动作排在退出前",
          acts.index("打开文件夹…") < acts.index("退出") < len(acts) - 1
          or acts[-1] == "退出", str(acts))
    check("调色台菜单已并入", "前后分割对比" in
          [a.text() for a in wb.menu("视图").actions()])
    check("工具菜单与标签同步",
          [a.text() for a in wb.menu("工具").actions()]
          == ["调色台", "对齐", "合成", "裁剪", "配乐", "运动"],
          str([a.text() for a in wb.menu("工具").actions()]))

    # 开工前：六个工具全部可用，空着也能进去手动选素材
    check("默认停在调色台", wb._active is dev, wb.windowTitle())
    check("未导入项目", wb.lb_material.text() == "未导入项目",
          wb.lb_material.text())
    check("空窗口显示起始页", dev.start.isVisible() and not dev.view.isVisible()
          and not dev.footer.isVisible(),
          f"start={dev.start.isVisible()} view={dev.view.isVisible()}")
    path, size = shoot(wb, "起始页")
    check("起始页截图", size > 10000, f"{os.path.basename(path)} {size // 1024}KB")
    check("六个工具一开始就能点",
          all(b.isEnabled() for b in wb.mod_btns.values()),
          str([(n, b.isEnabled()) for n, b in wb.mod_btns.items()]))
    check("未导入也能进合成页", wb.set_module(assemble.Assemble.name)
          and wb._active is asm, wb.lb_status.text())
    wb.set_module(developer.Developer.name)
    pump(app, 120)

    # 导入一个序列：项目名进顶栏，工具仍然全部可用
    d = make_series()
    wb.open_path(d)
    pump(app, 700)
    check("导入后停止选项卡在调色台", wb._active is dev, wb._active.name)
    check("导入后起始页让位", not dev.start.isVisible()
          and dev.view.isVisible() and dev.footer.isVisible())
    check("最近打开已记录", prefs.recent_projects()
          and prefs.recent_projects()[0] == os.path.normpath(d),
          str(prefs.recent_projects()[:2]))

    def cached_thumbs():
        root = prefs.thumb_cache_dir()
        return sum(1 for _b, _d, fs in os.walk(root) for f in fs
                   if f.endswith(".jpg"))
    t0 = time.time()
    while cached_thumbs() < 14 and time.time() - t0 < 8:
        pump(app, 100)
    check("缩略图写入磁盘缓存", cached_thumbs() >= 14,
          f"{cached_thumbs()} 个 JPEG")
    check("项目写进顶栏", wb.lb_material.text() == "序列 " + os.path.basename(d),
          wb.lb_material.text())
    check("窗口标题跟项目", wb.windowTitle().startswith(os.path.basename(d)),
          wb.windowTitle())
    check("导入序列后工具仍全开",
          all(b.isEnabled() for b in wb.mod_btns.values()),
          str([(n, b.isEnabled()) for n, b in wb.mod_btns.items()]))

    # 面板显隐：对齐/合成没有面板，调色台按子模式给
    check("对齐页面可见", wb.set_module(alg.name)
          and wb.pages.currentWidget() is wb.page_of[alg.name])
    pump(app, 120)
    check("对齐无停靠面板", visible_docks(wb) == [], str(visible_docks(wb)))
    check("对齐状态栏", wb.lb_status.text() == alg.hint(), wb.lb_status.text())
    wb.set_module(assemble.Assemble.name)
    pump(app, 120)
    check("合成无停靠面板", visible_docks(wb) == [], str(visible_docks(wb)))
    check("状态栏用模块提示", wb.lb_status.text() == asm.hint())
    check("切换标签勾选", wb.mod_btns["合成"].isChecked()
          and not wb.mod_btns["对齐"].isChecked())
    check("序列项目里也能进裁剪页",
          wb.set_module(cropper.Cropper.name)
          and wb._active is wb.module(cropper.Cropper.name),
          wb.lb_status.text())
    wb.set_module(assemble.Assemble.name)
    pump(app, 120)

    wb.set_module(developer.Developer.name)
    pump(app, 150)
    check("显影面板", set(visible_docks(wb)) == {"示波器", "调整", "胶片"},
          str(visible_docks(wb)))
    dev.set_sub_mode("导出")
    pump(app, 120)
    check("导出只留导出面板", visible_docks(wb) == ["导出"], str(visible_docks(wb)))
    wb.set_module(assemble.Assemble.name)
    pump(app, 120)
    check("切走后面板全收起", visible_docks(wb) == [], str(visible_docks(wb)))
    wb.set_module(developer.Developer.name)
    pump(app, 150)
    return wb, asm, dev


def test_pipeline(app, wb, asm, dev):
    """对齐/合成产出的序列 -> 合成 -> 成片自动进调色台。"""
    d = make_series()
    wb.material.set_series(d)
    # 手输路径有 500ms 防抖，流水线填目录也要等它
    t0 = time.time()
    while asm.series is None and time.time() - t0 < 10:
        pump(app, 50)
    check("序列自动填入合成目录", asm.ed_dir.text() == os.path.normpath(d))
    check("序列自动检测出 14 张", bool(asm.series) and asm.series["count"] == 14,
          str(asm.series["count"]) if asm.series else "未检出")
    check("调色台也拿到首帧", dev.work is not None and dev.browser.list.count() == 14,
          f"{dev.browser.list.count()} 张")

    # 同一个素材也要流到流水线第一站，别让对齐模块成为孤岛
    alg = wb.module(aligner.Aligner.name)
    t0 = time.time()
    while (alg.files is None or len(alg.files) != 14) and time.time() - t0 < 10:
        pump(app, 50)
    check("流水线：对齐也拿到序列",
          alg.ed_dir.text() == os.path.normpath(d) and alg.files
          and len(alg.files) == 14, alg.lb_info.text())

    asm.cb_side.setCurrentText("1280 (720p)")
    asm.cb_aspect.setCurrentText("16:9  UHD 4K 3840x2160")
    asm.cb_codec.setCurrentText("H.264 / x264（兼容性优先）")
    labels = [asm.cb_preset.itemText(i)
              for i in range(asm.cb_preset.count())]
    check("编码速度显示人话、值仍是 ffmpeg preset",
          all("ultra" not in s and "fast" not in s for s in labels)
          and "最快" in labels[0] and asm.cb_preset.findData("ultrafast") >= 0
          and asm.cb_preset.currentData() == "medium",
          f"{asm.cb_preset.currentText()} / {labels}")
    asm.cb_preset.setCurrentIndex(asm.cb_preset.findData("ultrafast"))
    asm.sl_crf.setValue(30)
    pump(app, 100)
    out = os.path.join(TMP, "wb_clip.mp4")
    asm.ed_out.setText(os.path.splitext(out)[0])
    check("输出路径含尺寸预览", "1280x720" in asm.lb_out.text(), asm.lb_out.text())
    # 截图顺手生成：主要模块各一张，改主题后老图不会留在 shots 里骗人
    wb.set_module(alg.name)
    pump(app, 250)
    shots = [shoot(wb, "工作台_对齐")]
    wb.set_module(asm.name)
    pump(app, 250)
    shots.append(shoot(wb, "工作台_合成"))
    wb.set_module(dev.name)
    dev.set_sub_mode("显影")
    pump(app, 400)
    shots.append(shoot(wb, "工作台_调色台"))
    check("外壳截图", all(s > 20000 for _, s in shots),
          "、".join(f"{os.path.basename(p)} {s // 1024}KB" for p, s in shots))

    # 皮肤：换肤要同时改 token、QSS、调色板和图标，浅色那套最容易露馅
    dark_bg = theme.TOK["BG"]
    bgs = {}
    canvas_ok = True
    for key in theme.SKINS:
        wb.set_skin(key)
        pump(app, 250)
        bgs[key] = theme.TOK["BG"]
        # 画布底色是设进去的刷子，最容易漏掉跟随皮肤
        canvas_ok &= (dev.view.backgroundBrush().color().name().upper()
                      == theme.TOK["CANVAS"].upper())
        shots.append(shoot(wb, f"皮肤_{key}"))
    check(f"{len(theme.SKINS)} 套皮肤都出图",
          len(shots[3:]) == len(theme.SKINS)
          and all(s > 20000 for _, s in shots[3:]),
          "、".join(f"{os.path.basename(p)} {s // 1024}KB" for p, s in shots[3:]))
    check("每套皮肤一套底色", len(set(bgs.values())) == len(bgs)
          and bgs["深空"] == dark_bg, "、".join(bgs.values()))
    check("画布跟随皮肤", canvas_ok)
    wb.set_skin("晨白")
    check("浅色皮肤生效", theme.TOK["dark"] is False
          and app.palette().window().color().lightness() > 150
          and theme.QSS.count(theme.TOK["BG"]) > 0, theme.TOK["BG"])
    check("换肤写进设置",
          wb._settings().value("shell/skin") == "晨白",
          str(wb._settings().value("shell/skin")))
    wb.set_skin("深空")
    check("换回深色", theme.TOK["dark"] is True
          and app.palette().window().color().lightness() < 40)

    ff = find_ffmpeg()
    check("找到 ffmpeg", bool(ff), ff or "")
    asm.run()
    # 14 帧太短，转一圈事件进度条就收起来了：启动瞬间直接断言，不等
    check("任务期间进度可见", wb.prog.isVisible() and wb.btn_cancel.isVisible())
    check("任务跑完", wait_job(app, wb))
    check("成片已写出", os.path.exists(out) and os.path.getsize(out) > 1000,
          str(os.path.getsize(out)) if os.path.exists(out) else "无文件")
    info = probe_video(ff, out) if os.path.exists(out) else None
    check("成片规格", info and info["w"] == 1280 and info["h"] == 720
          and abs(info["fps"] - 120) < 0.01,
          f"{info['w']}x{info['h']} {info['fps']}fps {info['dur']:.2f}s" if info else "")

    check("流水线：素材变成视频", wb.material.kind == "video"
          and os.path.normpath(wb.material.path) == os.path.normpath(out),
          wb.material.label())
    check("成片不改变项目类型",
          wb.project_kind == "series" and wb.lb_material.text().startswith("序列"),
          f"{wb.project_kind} {wb.lb_material.text()}")
    check("合成出片后合成/对齐仍可用",
          wb.mod_btns["合成"].isEnabled() and wb.mod_btns["对齐"].isEnabled()
          and wb.mod_btns["裁剪"].isEnabled(),
          str([(n, b.isEnabled()) for n, b in wb.mod_btns.items()]))
    check("流水线：调色台自动预览视频", dev.video is not None
          and dev.video["w"] == 1280, str(dev.video) if dev.video else "")
    pump(app, 800)
    check("流水线：预览已出图", not dev.view.after.pixmap().isNull())
    check("合成模块报告完成", asm.btn_open.isEnabled()
          and "全部完成" in asm.log_view.toPlainText(), wb.lb_status.text())
    # 状态栏只有一条，谁在前台谁说话：后台模块装载素材时不许抢
    wb.set_module(asm.name)
    dev.status("后台模块的一句话")
    check("后台模块不抢状态栏", "后台" not in wb.lb_status.text(), wb.lb_status.text())
    wb.set_module(dev.name)
    check("日志里有 ffmpeg 命令", "-progress pipe:1" in asm.log_view.toPlainText(),
          asm.log_view.toPlainText()[-120:].replace("\n", " "))
    return out


def test_drop(app, wb, video):
    """拖进窗口一条路径：素材换成它，并自动切到该去的模块。"""
    def do_drop(path):
        mime = QtCore.QMimeData()
        mime.setUrls([QtCore.QUrl.fromLocalFile(path)])
        enter = QtGui.QDragEnterEvent(
            QtCore.QPoint(20, 20), QtCore.Qt.DropAction.CopyAction, mime,
            QtCore.Qt.MouseButton.LeftButton,
            QtCore.Qt.KeyboardModifier.NoModifier)
        wb.dragEnterEvent(enter)
        drop = QtGui.QDropEvent(
            QtCore.QPointF(20, 20), QtCore.Qt.DropAction.CopyAction, mime,
            QtCore.Qt.MouseButton.LeftButton,
            QtCore.Qt.KeyboardModifier.NoModifier)
        wb.dropEvent(drop)
        return enter.isAccepted(), drop.isAccepted()

    ok_enter, ok_drop = do_drop(SERIES)
    pump(app, 800)          # 合成收到目录后要等 500ms 防抖扫描
    check("拖目录：导入项目并留在调色台",
          ok_enter and ok_drop and wb.material.kind == "series"
          and wb.project_kind == "series" and wb._active.name == "调色台",
          wb.material.label())
    check("拖目录：工具全部仍可用",
          all(b.isEnabled() for b in wb.mod_btns.values()),
          str([(n, b.isEnabled()) for n, b in wb.mod_btns.items()]))
    check("拖目录：合成识别出序列",
          wb.module("合成").series is not None
          and wb.module("合成").series["count"] == 14,
          wb.module("合成").lb_info.text())

    ok_enter, ok_drop = do_drop(video)
    wait_import(app, wb)
    pump(app, 500)
    check("拖视频：导入项目并留在调色台",
          ok_enter and ok_drop and wb.material.kind == "video"
          and wb.project_kind == "video" and wb._active.name == "调色台",
          wb.material.label())
    check("拖视频：工具全部仍可用",
          all(b.isEnabled() for b in wb.mod_btns.values()),
          str([(n, b.isEnabled()) for n, b in wb.mod_btns.items()]))
    check("拖视频：调色台能播放",
          wb.module("调色台").player.count > 1
          and wb.module("调色台").transport.isVisible(),
          f"{wb.module('调色台').player.count} 帧")
    check("拖视频：裁剪也已装载", wb.module("裁剪").info is not None,
          wb.module("裁剪").lb_info.text())

    img = os.path.join(ROOT, "assets", "app.png")
    ok_enter, ok_drop = do_drop(img)
    pump(app, 600)
    check("拖图片：接受并留在调色台",
          ok_enter and ok_drop and wb.material.kind == "image"
          and wb.project_kind == "image" and wb._active.name == "调色台",
          wb.material.label())
    check("拖图片：六个工具依然可用",
          all(b.isEnabled() for b in wb.mod_btns.values()),
          wb.lb_material.text())


def _mk_clip(path, w, h, extra_out=()):
    """造一条纯色小视频，用来验探测出来的几何尺寸。"""
    cmd = [find_ffmpeg(), "-y", "-loglevel", "error", "-f", "rawvideo",
           "-pix_fmt", "rgb24", "-s", f"{w}x{h}", "-r", "24", "-i", "-",
           "-c:v", "libx264", "-pix_fmt", "yuv420p", *extra_out, path]
    with subprocess.Popen(cmd, stdin=subprocess.PIPE) as p:
        for _ in range(12):
            p.stdin.write(np.full((h, w, 3), 128, np.uint8).tobytes())
    return path


def test_geometry():
    """非方形像素：预览与静帧要按显示尺寸解，不是比特流里的存储尺寸。

    竖拍机内压成 4:3 存储、播放时拉回 16:9 的素材最常见，SAR 不解析
    就会把画面压扁；旋转元数据这边 ffmpeg 自己转，本机造不出带 rotate 的
    片子，所以只验文案分支。
    """
    ana = _mk_clip(os.path.join(TMP, "wb_anamorphic.mp4"), 640, 360,
                   ["-vf", "setsar=16/9"])
    info = probe_video(find_ffmpeg(), ana)
    check("探到 SAR", tuple(info["sar"]) == (16, 9), str(info["sar"]))
    check("存储尺寸不动、显示尺寸拉开",
          (info["w"], info["h"]) == (640, 360)
          and ffmod.disp_size(info) == (1138, 360),
          f"{info['w']}x{info['h']} -> {ffmod.disp_size(info)}")
    check("信息条把原因写出来",
          "1138×360" in ffmod.disp_text(info)
          and "存储 640×360" in ffmod.disp_text(info)
          and "16:9" in ffmod.disp_text(info), ffmod.disp_text(info))
    check("非方形像素不许走比特流无损", info["crop_ok"] is False)

    src = playback.VideoSource(find_ffmpeg(), ana, info, long_side=1280)
    r = src.open(0)
    check("播放解码按显示比例",
          abs(r.w / r.h - 1138 / 360) < 0.01, f"{r.w}x{r.h}")
    r.close()
    png = ffmod.extract_frame(find_ffmpeg(), ana, 0.05,
                              dst=ffmod.disp_size(info))
    im = cv2.imdecode(np.frombuffer(png, np.uint8), cv2.IMREAD_COLOR) if png else None
    check("静帧按显示尺寸解", im is not None and (im.shape[1], im.shape[0]) == (1138, 360),
          f"{None if im is None else (im.shape[1], im.shape[0])}")

    sq = _mk_clip(os.path.join(TMP, "wb_square.mp4"), 640, 360)
    s = probe_video(find_ffmpeg(), sq)
    check("方形像素不无端加显示尺寸",
          tuple(s["sar"]) == (1, 1) and ffmod.disp_size(s) == (640, 360)
          and ffmod.disp_text(s) == "640×360" and s["crop_ok"],
          f"sar={s['sar']} {ffmod.disp_text(s)} crop_ok={s['crop_ok']}")
    # 旋转只能验文案：显示尺寸是探测阶段算好的，这里给一份合成字典
    rot = {"w": 1080, "h": 1920, "sar": (1, 1), "angle": 90,
           "dw": 1920, "dh": 1080}
    check("旋转写进信息条", "旋转 90°" in ffmod.disp_text(rot)
          and ffmod.disp_size(rot) == (1920, 1080), ffmod.disp_text(rot))


def test_reopen(app, wb, video):
    """视频 -> 图片 -> 再打开同一条视频：去重不许吞掉用户主动的重新导入。

    Material._set 的去重是给流水线用的（对齐写完给合成，合成别再回踢对齐）；
    从导入入口进来是用户主动点的，同一路径也必须真的重载一次。
    """
    dev = wb.module("调色台")
    wb.open_path(video)
    wait_import(app, wb)
    pump(app, 800)
    check("重导入前视频有画面",
          dev.video is not None and dev.player.count > 1
          and not dev.view.after.pixmap().isNull(),
          f"{dev.player.count} 帧")

    wb.open_path(os.path.join(ROOT, "assets", "app.png"))
    pump(app, 600)
    check("换成图片后视频状态清空",
          wb.material.kind == "image" and dev.video is None,
          str(dev.video))

    wb.open_path(video)
    wait_import(app, wb)
    pump(app, 800)
    check("再导入同一条视频：素材跟着换回视频",
          wb.material.kind == "video"
          and os.path.normpath(wb.material.path) == os.path.normpath(video),
          wb.material.label())
    check("再导入同一条视频：画面回来了",
          dev.video is not None and dev.player.count > 1
          and not dev.view.after.pixmap().isNull(),
          f"{dev.player.count} 帧  画布{'空' if dev.view.after.pixmap().isNull() else '有图'}")

    # 用户报的那条路径：胶片条里点图片（不动 Material）→ 回到视频 → 重导同一条
    dev.load_media(os.path.join(ROOT, "assets", "app.png"))
    pump(app, 500)
    check("胶片条点图片：画面换成图片、素材仍是视频",
          dev.video is None and wb.material.kind == "video",
          f"kind={wb.material.kind} video={dev.video}")
    wb.open_path(video)
    wait_import(app, wb)
    pump(app, 800)
    check("重开同一条视频：不被去重吞掉，画面回到视频",
          dev.video is not None and dev.player.count > 1
          and not dev.view.after.pixmap().isNull(),
          f"{dev.player.count} 帧 画布{'空' if dev.view.after.pixmap().isNull() else '有图'}")


def test_import_async(app, wb, video):
    """导入视频不许冻住主线程，而且只探测一次。

    以前四个模块各自 probe + 解一张全分辨率静帧，全串在主线程上：一条 4K
    片子点完"打开"到能动鼠标要八九秒。现在外壳起一个后台导入，跑完才发布
    素材，各模块拿到的是同一份缓存。
    """
    dev = wb.module("调色台")
    ffmod.clear_bundles()
    calls = []
    real_probe = ffmod.probe_video
    ffmod.probe_video = lambda f, p: (calls.append(p), real_probe(f, p))[1]
    try:
        wb.open_path(os.path.join(ROOT, "assets", "app.png"))
        pump(app, 500)
        t0 = time.time()
        wb.open_path(video)
        elapsed = time.time() - t0
        check("点开通用素材不卡主线程", elapsed < 0.6,
              f"open_path 返回用了 {elapsed:.2f}s")
        check("导入期间进度条可见",
              wb.prog.isVisible() and wb.btn_cancel.isVisible(),
              f"prog={wb.prog.isVisible()} 取消={wb.btn_cancel.isVisible()}")
        check("导入期间状态栏写明在导什么",
              "正在导入" in wb.lb_status.text() and os.path.basename(video) in wb.lb_status.text(),
              wb.lb_status.text())
        done = wait_import(app, wb)
        pump(app, 500)
    finally:
        ffmod.probe_video = real_probe
    check("一次导入只探测一次", done and len(calls) == 1,
          f"probe_video 被叫了 {len(calls)} 次")
    check("导入完成才发布素材",
          wb.material.kind == "video"
          and os.path.normpath(wb.material.path) == os.path.normpath(video),
          wb.material.label())
    check("导入完成后进度条收起",
          not wb.prog.isVisible() and not wb.btn_cancel.isVisible(),
          f"prog={wb.prog.isVisible()} 取消={wb.btn_cancel.isVisible()}")
    check("导入结果进了缓存", ffmod.cached_bundle(video) is not None)
    check("导入后调色台有画面",
          dev.video is not None and not dev.view.after.pixmap().isNull(),
          str(dev.video) if dev.video else "无")

    # 再导同一条：命中缓存就在主线程直接发布，不该再转一圈 ffmpeg
    wb.open_path(os.path.join(ROOT, "assets", "app.png"))
    pump(app, 400)
    n = len(calls)
    t0 = time.time()
    wb.open_path(video)
    pump(app, 300)
    check("重复导入同一条走缓存、当场发布",
          wb.material.kind == "video" and len(calls) == n
          and wb._import is None,
          f"kind={wb.material.kind} 新增探测 {len(calls) - n} 次 "
          f"耗时 {time.time() - t0:.2f}s")

    # 中途取消：半张预览不许冒充成品缓存，否则之后每次装载都缺画面
    ffmod.clear_bundles()
    half = ffmod.import_bundle(find_ffmpeg(), video, stopping=lambda: True)
    check("取消在半路的导入不进缓存",
          half is not None and half["png"] is None
          and ffmod.cached_bundle(video) is None,
          f"png={None if not half else bool(half['png'])} "
          f"缓存={ffmod.cached_bundle(video) is not None}")
    ffmod.clear_bundles()


def test_thumb_cache():
    """磁盘缩略图缓存：能落盘、能回读、原图变了自动失效。"""
    src = os.path.join(TMP, "cache_src.jpg")
    rgb = np.zeros((80, 120, 3), np.uint8)
    rgb[..., 0], rgb[..., 1], rgb[..., 2] = 20, 120, 200
    cv2.imwrite(src, cv2.cvtColor(rgb, cv2.COLOR_RGB2BGR))

    p1 = filmstrip._disk_path(src, 128)
    check("缩略图缓存路径", bool(p1) and p1.endswith(".jpg"), str(p1))
    filmstrip._save_disk(p1, rgb)
    check("缩略图缓存落盘", bool(p1) and os.path.isfile(p1),
          str(os.path.getsize(p1)) + "B" if p1 and os.path.exists(p1) else "无")
    back = filmstrip._load_disk(p1)
    check("缩略图缓存可回读", back is not None and back.shape == rgb.shape
          and abs(int(back[..., 2].mean()) - 200) < 12,
          f"{back.shape} 蓝均值 {back[..., 2].mean():.0f}" if back is not None else "读不到")

    st = os.stat(src)
    os.utime(src, ns=(st.st_atime_ns, st.st_mtime_ns + 10 ** 9))
    p2 = filmstrip._disk_path(src, 128)
    check("原图改动后缓存失效", p2 != p1, f"{p1} -> {p2}")

    prefs.put("cache/thumbs", False)
    check("关掉缩略图缓存后不落盘", filmstrip._disk_path(src, 128) is None)
    prefs.put("cache/thumbs", True)


def _plugin_item(dlg, key):
    for i in range(dlg.pl_list.count()):
        item = dlg.pl_list.item(i)
        if item.data(QtCore.Qt.ItemDataRole.UserRole) == key:
            return item
    return None


def test_settings(app, wb, dev):
    """设置对话框 + 插件加载/卸载 + 缓存清理。"""
    from reference.gui import settingsdialog

    check("顶栏有设置按钮", wb.btn_settings.isVisible()
          and "设置" in wb.btn_settings.toolTip(),
          wb.btn_settings.toolTip())
    acts = [a.text() for a in wb.menu("文件").actions()]
    check("文件菜单有设置项", "设置…" in acts
          and wb.act_settings.shortcut().toString() == "Ctrl+,",
          f"{wb.act_settings.shortcut().toString()} {acts}")

    dlg = settingsdialog.SettingsDialog(wb)
    dlg.show()
    pump(app, 120)
    check("设置五个分页", dlg.nav.count() == 5 and dlg.stack.count() == 5,
          f"{dlg.nav.count()}/{dlg.stack.count()}")
    dlg.nav.setCurrentRow(1)
    pump(app, 120)
    shot = os.path.join(SHOTS, "shot_设置.png")
    dlg.grab().save(shot)
    check("设置页截图", os.path.getsize(shot) > 10000,
          f"{os.path.basename(shot)} {os.path.getsize(shot) // 1024}KB")
    dlg.nav.setCurrentRow(0)

    # ---- 常规 ----
    dlg.sp_recent.setValue(3)
    check("最近条数写入并裁剪", int(prefs.get("general/recent_count")) == 3
          and len(prefs.recent_projects()) <= 3,
          f"{prefs.get('general/recent_count')} / {len(prefs.recent_projects())}")
    dlg.sp_recent.setValue(8)
    dlg.cb_restore.setChecked(False)
    check("恢复上次项目开关", prefs.as_bool(
        prefs.get("general/restore_last"), True) is False)
    dlg.cb_restore.setChecked(True)

    # ---- 缓存 ----
    cache = os.path.join(TMP, "set_cache")
    dlg.ed_cache.setText(cache)
    dlg._apply_cache_dir()
    check("缓存目录生效", prefs.cache_dir() == os.path.normpath(cache),
          prefs.cache_dir())
    os.makedirs(prefs.cache_dir(), exist_ok=True)
    junk = os.path.join(prefs.cache_dir(), "junk.bin")
    with open(junk, "wb") as f:
        f.write(b"x" * 4096)
    ask = QtWidgets.QMessageBox.question
    QtWidgets.QMessageBox.question = staticmethod(
        lambda *a, **k: QtWidgets.QMessageBox.StandardButton.Yes)
    try:
        dlg._clear_cache()
    finally:
        QtWidgets.QMessageBox.question = ask
    check("清理缓存只清缓存目录", not os.path.exists(junk)
          and os.path.isdir(prefs.cache_dir()), dlg.lb_cache_size.text())
    dlg.cb_thumbs.setChecked(False)
    check("缩略图缓存开关落盘", prefs.thumb_cache_enabled() is False)
    dlg.cb_thumbs.setChecked(True)

    # ---- 界面 ----
    before = dlg.cb_skin.currentData()
    idx = dlg.cb_skin.findData("晨白")
    dlg.cb_skin.setCurrentIndex(idx)
    pump(app, 150)
    check("设置里换肤立即生效", theme.SKIN == "晨白"
          and wb.skin_acts["晨白"].isChecked(), theme.SKIN)
    dlg.cb_skin.setCurrentIndex(dlg.cb_skin.findData(before))
    pump(app, 150)
    check("换回原皮肤", theme.SKIN == before, theme.SKIN)
    dlg.cb_geometry.setChecked(False)
    check("记住窗口开关落盘", prefs.as_bool(
        prefs.get("window/remember"), True) is False)
    dlg.cb_geometry.setChecked(True)

    # 窗口弧线：改了要立刻重贴给可见窗口，并且写进设置能在下次启动恢复
    dlg.cb_corner.setCurrentIndex(dlg.cb_corner.findData(3))
    pump(app, 120)
    check("设置里换窗口弧线立即生效", theme.CORNER == 3
          and int(wb._settings().value("shell/corner")) == 3,
          str(wb._settings().value("shell/corner")))
    if sys.platform == "win32":
        check("系统接受窗口弧线属性", theme.CORNER_HR == 0,
              f"hr=0x{(theme.CORNER_HR or 0) & 0xFFFFFFFF:08x}")
    check("弧线说明跟着档位走", "小圆角" in dlg.lb_corner.text()
          or "不认" in dlg.lb_corner.text(), dlg.lb_corner.text())
    dlg.cb_corner.setCurrentIndex(dlg.cb_corner.findData(2))
    pump(app, 120)
    check("换回大圆角", theme.CORNER == 2
          and int(wb._settings().value("shell/corner")) == 2)

    # 字号：改一档要整屏立刻跟着变（QSS 是重新生成的），并写进设置
    qss_before = theme.QSS
    dlg.cb_font.setCurrentIndex(dlg.cb_font.findData("特大"))
    pump(app, 150)
    check("设置里换字号立即生效", theme.FONT == "特大"
          and theme.fs(15) == 20 and theme.QSS != qss_before
          and "font-size: 20px" in theme.QSS,
          f"{theme.FONT} 正文 {theme.fs(15)}px")
    check("字号写进设置", wb._settings().value("shell/font") == "特大",
          str(wb._settings().value("shell/font")))
    # 截图留给肉眼复核：字号档位这种东西，断言只能证明数值变了
    wb.set_module(dev.name)
    dev.set_sub_mode("显影")
    pump(app, 300)
    p_big, s_big = shoot(wb, "字号_特大")
    dlg.cb_font.setCurrentIndex(dlg.cb_font.findData("紧凑"))
    pump(app, 150)
    check("换到紧凑档字变小", theme.FONT == "紧凑" and theme.fs(15) == 13,
          f"正文 {theme.fs(15)}px")
    _p_small, s_small = shoot(wb, "字号_紧凑")
    check("字号对比出图", s_big > 20000 and s_small > 20000,
          f"{os.path.basename(p_big)} {s_big // 1024}KB / {s_small // 1024}KB")
    dlg.cb_font.setCurrentIndex(dlg.cb_font.findData("标准"))
    pump(app, 150)
    check("换回标准档", theme.FONT == "标准" and theme.fs(15) == 15,
          str(wb._settings().value("shell/font")))

    # 折叠分组的空档：面板剩余高度要归最后那条伸缩，不归分组本身
    lay = dev.tools.layout()
    tail = lay.itemAt(lay.count() - 1)
    for g in dev.groups:
        g.set_collapsed(True)
    pump(app, 200)
    check("折叠后分组只占标题那一行",
          tail.spacerItem() is not None
          and all(g.height() <= g.header.height() + 1 for g in dev.groups),
          f"末项={type(tail).__name__} 组高="
          f"{[(g.height(), g.header.height()) for g in dev.groups[:3]]}")
    shoot(wb, "右栏折叠不留空档")
    for g in dev.groups:
        g.set_collapsed(False)
    pump(app, 200)

    # ---- 插件 ----
    pdir = prefs.plugin_dir()
    shutil.rmtree(pdir, ignore_errors=True)
    os.makedirs(pdir, exist_ok=True)
    with open(os.path.join(pdir, "demo_plugin.py"), "w", encoding="utf-8") as f:
        f.write('NAME = "测试插件"\nVERSION = "1.0"\n'
                'DESCRIPTION = "只加一个菜单动作"\n'
                'def register(api):\n'
                '    api.add_action("工具", "插件打个招呼",'
                ' lambda: api.notify("插件已运行"))\n')
    with open(os.path.join(pdir, "boom_plugin.py"), "w", encoding="utf-8") as f:
        f.write('NAME = "爆炸插件"\nVERSION = "1.0"\n'
                'def register(api):\n    pass\n'
                'raise RuntimeError("插件顶层不该执行")\n')
    with open(os.path.join(pdir, "bad_meta.py"), "w", encoding="utf-8") as f:
        f.write('NAME = "缺入口"\nVERSION = "1.0"\n')

    dlg._scan_plugins()
    check("插件目录扫描", dlg.pl_list.count() == 3, str(dlg.pl_list.count()))
    bad = _plugin_item(dlg, "bad_meta")
    check("坏插件标出错误且不可启用", bad is not None
          and "加载失败" in bad.text()
          and not (bad.flags() & QtCore.Qt.ItemFlag.ItemIsUserCheckable),
          bad.text() if bad else "未列出")
    # 禁用状态下不执行插件顶层代码：boom 能扫出来说明扫描只解析 AST
    boom = _plugin_item(dlg, "boom_plugin")
    check("禁用插件不被执行", boom is not None and not boom.text().endswith("加载失败"),
          boom.text() if boom else "未列出")

    demo = _plugin_item(dlg, "demo_plugin")
    check("插件默认关闭", demo is not None
          and demo.checkState() == QtCore.Qt.CheckState.Unchecked)
    demo.setCheckState(QtCore.Qt.CheckState.Checked)
    pump(app, 150)
    tools = [a.text() for a in wb.menu("工具").actions()]
    check("启用插件后动作进菜单", "插件打个招呼" in tools, str(tools))
    check("启用状态写盘", pluginmod.is_enabled(
        pluginmod.Plugin(os.path.join(pdir, "demo_plugin.py"))))
    demo.setCheckState(QtCore.Qt.CheckState.Unchecked)
    pump(app, 150)
    check("关闭插件后动作移除",
          "插件打个招呼" not in [a.text() for a in wb.menu("工具").actions()])

    boom.setCheckState(QtCore.Qt.CheckState.Checked)
    pump(app, 150)
    check("插件运行错误被隔离", "不该执行" in dlg.lb_plugin_state.text()
          and "插件加载失败" in wb.lb_status.text(),
          dlg.lb_plugin_state.text())
    boom.setCheckState(QtCore.Qt.CheckState.Unchecked)
    pump(app, 100)

    # ---- 关于 ----
    about = dlg.stack.widget(4)
    texts = [lb.text() for lb in about.findChildren(QtWidgets.QLabel)]
    check("关于页有版本与运行环境", any("版本" in s for s in texts)
          and any("PySide6" in s for s in texts), " / ".join(texts[:4]))
    dlg.close()

    # 设置改完能重开：说明写盘与初始化都稳定
    dlg2 = settingsdialog.SettingsDialog(wb)
    check("设置可重复打开", dlg2.cb_skin.currentData() == theme.SKIN
          and int(dlg2.cb_corner.currentData()) == theme.CORNER
          and dlg2.cb_font.currentData() == theme.FONT
          and prefs.cache_dir() == os.path.normpath(cache), prefs.cache_dir())
    dlg2.close()


def test_edits(app, dev):
    """参数旁车：校验、原子写、防抖自动保存、切项目恢复/复位。"""
    base = gc.defaults()
    base["exposure"] = 1.5
    base["curve_rgb"] = [[0, 12], [128, 140], [255, 250]]
    base["masks"] = [dict(gc.new_mask("radial"), x1=0.4, y1=0.5, x2=0.6,
                          y2=0.55, angle=33.0, midpoint=62.0, amount=80.0)]
    base["masks"][0]["params"] = {"exposure": 0.8, "saturation": 12.0}
    key = "series:" + os.path.join(TMP, "edits_probe")
    check("旁车写入", edits.save(key, base))
    back = edits.load(key)
    check("旁车往返保留曲线与蒙版",
          back is not None and abs(back["exposure"] - 1.5) < 1e-6
          and back["curve_rgb"] == base["curve_rgb"]
          and back["masks"][0]["components"][0]["angle"] == 33.0
          and back["masks"][0]["id"] == base["masks"][0]["id"]
          and abs(back["masks"][0]["amount"] - 80.0) < 1e-6
          and abs(back["masks"][0]["params"]["exposure"] - 0.8) < 1e-6,
          "" if back is None else f"蒙版 {len(back['masks'])}")
    bad = {"exposure": float("nan"), "mono_h": [1, 2], "curve_r": "x",
           "masks": [{"type": "radial", "x1": 9, "amount": 500,
                      "params": {"exposure": "坏", "nope": 1}},
                     {"type": "不存在的蒙版"}], "surprise": 1}
    q = edits.sanitize(bad)
    check("坏参数被拦回安全值", abs(q["exposure"]) < 1e-9
          and len(q["mono_h"]) == 8 and q["curve_r"] == [[0, 0], [255, 255]]
          and len(q["masks"]) == 1
          and q["masks"][0]["components"][0]["x1"] == 1.0
          and q["masks"][0]["amount"] == 100.0
          and "nope" not in q["masks"][0]["params"], str(q["masks"]))
    rp = edits.record_path(key)
    with open(rp, "w", encoding="utf-8") as f:
        f.write("{不是 JSON")
    check("坏旁车文件只当没记录", edits.load(key) is None)
    check("旁车不留临时文件",
          not os.path.exists(rp + ".tmp"))

    # 自动保存：改成 700ms 防抖，清空后删掉记录
    d = os.path.join(TMP, "edits_series")
    os.makedirs(d, exist_ok=True)
    for f in os.listdir(d):
        os.remove(os.path.join(d, f))
    src = []
    for i in range(2):
        p = os.path.join(d, f"e_{i}.jpg")
        cv2.imwrite(p, np.full((60, 80, 3), 90 + i * 30, np.uint8))
        src.append(p)
    dev.set_media_list(src)
    pump(app, 400)
    dev.store.set("exposure", 1.25)
    pump(app, 1100)
    rec = edits.load(dev._edit_key)
    check("改了参数自动落盘", rec is not None
          and abs(rec["exposure"] - 1.25) < 1e-6,
          f"{rec and rec['exposure']}")
    saved_key = dev._edit_key
    dev.store.set("exposure", 0.0)
    pump(app, 1100)
    check("复位后清掉旁车", not os.path.exists(edits.record_path(saved_key)))

    dev.store.set("exposure", -0.75)
    pump(app, 1100)
    dev.store.load(gc.defaults())        # 模拟重启后的空参数
    dev._edit_key = None                 # 强制走一次项目切换
    dev.set_media_list(src)
    pump(app, 400)
    check("重开序列恢复参数", abs(dev.store.value("exposure") + 0.75) < 1e-6,
          f"{dev.store.value('exposure'):+.2f}")

    # 去闪烁旁车：按帧名单核对，序列变了旧校正就不能再生效
    fkey = dev._edit_key
    fdata = {"files": [os.path.basename(p) for p in src],
             "offsets": [0.25, -0.35], "means": [0.1, 0.12],
             "window": 9, "strength": 80}
    check("去闪烁旁车写入", edits.save_deflicker(fkey, fdata))
    back = edits.load_deflicker(fkey, src)
    check("去闪烁旁车往返", back is not None
          and back["offsets"] == [0.25, -0.35] and back["window"] == 9
          and back["strength"] == 80 and back["files"] == fdata["files"],
          str(back and back["offsets"]))
    check("帧名单变了不认旧校正",
          edits.load_deflicker(fkey, src[::-1]) is None)
    check("去闪烁不留临时文件",
          not os.path.exists(edits.deflicker_path(fkey) + ".tmp"))
    edits.clear_deflicker(fkey)
    check("去闪烁记录可清除", edits.load_deflicker(fkey, src) is None)

    # 关键帧旁车：逐帧参数快照 + 开关，帧名单变了就不认
    kf0 = gc.defaults()
    kf0["exposure"] = -1.0
    kf1 = gc.defaults()
    kf1["exposure"] = 2.0
    kdata = {"files": [os.path.basename(p) for p in src], "on": True,
             "keyframes": [{"index": 1, "params": kf1},
                           {"index": 0, "params": kf0}]}
    check("关键帧旁车写入", edits.save_keyframes(fkey, kdata))
    kb = edits.load_keyframes(fkey, src)
    check("关键帧旁车往返", kb is not None and kb["on"] is True
          and [k["index"] for k in kb["keyframes"]] == [0, 1]
          and abs(kb["keyframes"][1]["params"]["exposure"] - 2.0) < 1e-6,
          str(kb and [k["index"] for k in kb["keyframes"]]))
    check("帧名单变了不认关键帧",
          edits.load_keyframes(fkey, src[::-1]) is None)
    check("关键帧不留临时文件",
          not os.path.exists(edits.keyframes_path(fkey) + ".tmp"))
    edits.clear_keyframes(fkey)
    check("关键帧记录可清除", edits.load_keyframes(fkey, src) is None)

    single = os.path.join(TMP, "edits_single", "one.jpg")
    os.makedirs(os.path.dirname(single), exist_ok=True)
    cv2.imwrite(single, np.full((60, 80, 3), 140, np.uint8))
    dev.set_media_list([single])
    pump(app, 400)
    check("新项目无记录就复位", abs(dev.store.value("exposure")) < 1e-6,
          f"{dev.store.value('exposure'):+.2f}")


def test_deflicker(app, dev):
    """测光→应用→预览/批量参数→旁车重载→开关→清除。"""
    d = os.path.join(TMP, "flicker_seq")
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d, exist_ok=True)
    gains = [1.0, 1.5, 0.85, 1.35, 0.9, 1.2, 1.0, 1.1, 0.95, 1.05]
    paths = []
    for i, g in enumerate(gains):
        lin = min(0.12 * g * (1.0 + i * 0.008), 1.0)
        s = 12.92 * lin if lin <= 0.0031308 else \
            1.055 * lin ** (1 / 2.4) - 0.055
        p = os.path.join(d, f"f_{i:03d}.png")
        cv2.imwrite(p, np.full((80, 120, 3), int(round(s * 255)), np.uint8))
        paths.append(p)
    dev.set_media_list(paths)
    pump(app, 500)

    res = flicker.scan(paths, 9, 100)
    check("序列测光产生校正量", res is not None
          and max(abs(x) for x in res["offsets"]) > 0.2,
          f"max {max(abs(x) for x in (res or {'offsets': [0]})['offsets']):.2f}")
    data = {"files": [os.path.basename(p) for p in paths],
            "offsets": res["offsets"], "means": res["means"],
            "window": 9, "strength": 100}
    dev.set_deflicker(data, persist=True)
    pump(app, 400)
    off1 = res["offsets"][1]
    check("应用后预览套用当前帧偏移",
          abs(dev._preview_params(1)["exposure"] - off1) < 1e-6,
          f"{dev._preview_params(1)['exposure']:+.3f}")

    dev.store.set("exposure", 0.3)
    pump(app, 300)
    off2 = res["offsets"][2]
    check("偏移与用户曝光相加",
          abs(dev._preview_params(2)["exposure"] - (0.3 + off2)) < 1e-6,
          f"{dev._preview_params(2)['exposure']:+.3f}")
    check("用户参数本身不被改写",
          abs(dev.store.value("exposure") - 0.3) < 1e-9)
    at = dev._batch_params()
    check("批量导出逐帧参数", at is not None
          and abs(at(paths[1])["exposure"] - (0.3 + off1)) < 1e-6
          and abs(at(paths[2])["exposure"] - (0.3 + off2)) < 1e-6,
          "每帧偏移不同")
    back = edits.load_deflicker(dev._edit_key, paths)
    check("去闪烁校正自动落盘", back is not None
          and back["offsets"] == res["offsets"] and back["window"] == 9,
          f"{back and len(back['offsets'])} 帧")

    dev.cb_flicker.setChecked(False)
    pump(app, 300)
    check("关掉应用后回到基础曝光",
          abs(dev._preview_params(1)["exposure"] - 0.3) < 1e-9
          and dev._batch_params() is None)
    dev.cb_flicker.setChecked(True)
    pump(app, 300)
    check("重新勾选恢复校正",
          abs(dev._preview_params(1)["exposure"] - (0.3 + off1)) < 1e-6)

    # 模拟重启：清空内存后重走一次项目切换
    dev._deflicker = None
    dev._deflicker_on = False
    dev._flicker_sig = None
    dev._switch_edits(paths[0])
    pump(app, 300)
    check("重开序列从旁车恢复去闪烁",
          dev.current_deflicker() is not None
          and abs(dev._preview_params(1)["exposure"] - (0.3 + off1)) < 1e-6,
          f"{dev._preview_params(1)['exposure']:+.3f}")

    dev.clear_deflicker()
    dev.store.set("exposure", 0.0)
    pump(app, 300)
    check("清除后校正与记录都消失",
          dev.current_deflicker() is None and dev._batch_params() is None
          and not os.path.exists(edits.deflicker_path(dev._edit_key)))

    # 对话框走真实后台任务与全局取消按钮
    dlg = flicker_ui.DeflickerDialog(dev, dev.photo_items())
    dlg.show()
    pump(app, 100)
    dlg._analyze()
    t0 = time.time()
    while dlg._job is not None and time.time() - t0 < 15:
        pump(app, 100)
    check("去闪烁对话框后台分析完成",
          dlg._job is None and bool(dlg._offsets) and dlg.btn_apply.isEnabled(),
          f"{len(dlg._offsets or [])} 帧")
    check("对话框画出偏移与统计",
          len(dlg.canvas._rows) == len(paths)
          and "P95" in dlg.lb_stats.text(), dlg.lb_stats.text())
    check("分析结束进度条收起", not dev.shell.prog.isVisible())
    dlg._apply()
    pump(app, 300)
    check("对话框应用后立即生效",
          dev.current_deflicker() is not None
          and abs(dev._preview_params(1)["exposure"]
                  - (dlg._offsets or [0])[1]) < 1e-6)
    check("对话框应用后落盘",
          edits.load_deflicker(dev._edit_key, paths) is not None)
    check("标量记录读回来仍按整帧亮度",
          (lambda b: b is not None and not b["channels"] and not b["multi"]
           and all(not isinstance(o, (list, tuple)) for o in b["offsets"]))(
               edits.load_deflicker(dev._edit_key, paths)))
    # 整帧亮度的测光值换成分通道：不能沿用，得清掉让重扫
    dlg2 = flicker_ui.DeflickerDialog(dev, dev.photo_items())
    dlg2.show()
    pump(app, 100)
    check("旧测光是每帧一个数",
          dlg2._means and not isinstance(dlg2._means[0], (list, tuple)))
    dlg2.cb_channels.setChecked(True)
    check("换测光种类后要求重新分析",
          not dlg2._means and not dlg2.btn_apply.isEnabled()
          and "重新分析" in dlg2.lb_state.text(), dlg2.lb_state.text())
    dlg2.close()
    dev.clear_deflicker()
    pump(app, 200)

    # 分通道 + 两遍：彩色闪烁（亮度基本不动，R/B 反向跳）
    cd = os.path.join(TMP, "flicker_chan")
    shutil.rmtree(cd, ignore_errors=True)
    os.makedirs(cd, exist_ok=True)
    cpaths = []
    for i in range(10):
        alt = 1 if i % 2 else -1
        px = np.zeros((60, 90, 3), np.uint8)
        px[...] = (int(120 - alt * 26), 120, int(120 + alt * 26))
        p = os.path.join(cd, f"c_{i:03d}.png")
        cv2.imwrite(p, px)
        cpaths.append(p)
    dev.set_media_list(cpaths)
    pump(app, 500)
    dlg3 = flicker_ui.DeflickerDialog(dev, dev.photo_items())
    dlg3.show()
    pump(app, 100)
    dlg3.cb_channels.setChecked(True)
    dlg3.cb_multi.setChecked(True)
    dlg3._analyze()
    t0 = time.time()
    while dlg3._job is not None and time.time() - t0 < 20:
        pump(app, 100)
    offs3 = dlg3._offsets or []
    check("分通道后台分析出每帧三个偏移",
          dlg3._job is None and len(offs3) == len(cpaths)
          and all(len(o) == 3 for o in offs3), f"{len(offs3)} 帧")
    check("曲线画成三条并给出色闪统计",
          dlg3.canvas._chan and len(dlg3.canvas._rows[0]) == 3
          and "色闪" in dlg3.lb_stats.text(), dlg3.lb_stats.text())
    check("红蓝补的方向相反",
          offs3[1][0] * offs3[1][2] < 0,
          f"R {offs3[1][0]:+.2f} / B {offs3[1][2]:+.2f}")
    # 三条曲线的样子由测试拍下来，别留手工截图
    shot = os.path.join(SHOTS, "shot_去闪烁_分通道.png")
    dlg3.grab().save(shot)
    check("分通道曲线截图", os.path.getsize(shot) > 40000,
          f"{os.path.getsize(shot) // 1024}KB")
    dlg3._apply()
    pump(app, 300)
    cur = dev.current_deflicker()
    check("分通道校正生效并记下设置",
          cur is not None and cur["channels"] and cur["multi"],
          str(cur and (cur["channels"], cur["multi"])))
    pv = dev._preview_params(1)
    check("分通道时挂 chan_ev 而不是并进曝光",
          isinstance(pv.get("chan_ev"), list) and len(pv["chan_ev"]) == 3
          and abs(pv["chan_ev"][0] - pv["chan_ev"][2]) > 0.05,
          str(pv.get("chan_ev")))
    back3 = edits.load_deflicker(dev._edit_key, cpaths)
    check("分通道旁车回读三个数一组",
          back3 is not None and back3["channels"] and back3["multi"]
          and all(len(o) == 3 for o in back3["offsets"])
          and len(back3["means"][0]) == 3, str(back3 and back3["offsets"][1]))
    # 三通道补得一样时回到老路径：并进曝光，不挂 chan_ev
    base_exp = float(dev.store.value("exposure"))
    flat = [[0.4, 0.4, 0.4] for _ in cpaths]
    dev.set_deflicker({"files": [os.path.basename(p) for p in cpaths],
                       "offsets": flat, "means": [[0.2, 0.2, 0.2]] * len(cpaths),
                       "window": 9, "strength": 100,
                       "channels": True, "multi": False}, persist=True)
    pump(app, 300)
    pf = dev._preview_params(1)
    check("等值分通道并进曝光",
          abs(pf["exposure"] - (base_exp + 0.4)) < 1e-9
          and "chan_ev" not in pf,
          f"{pf['exposure']:+.2f} / {pf.get('chan_ev')}")
    check("批量导出也按分通道取每帧参数",
          (lambda at: at is not None and abs(at(cpaths[1])["exposure"]
                                             - (base_exp + 0.4)) < 1e-9)(
              dev._batch_params()))
    # 越界的偏移被旁车夹住，套用前也再夹一次
    dev.clear_deflicker()
    dev.set_deflicker({"files": [os.path.basename(p) for p in cpaths],
                       "offsets": [[9.0, -9.0, 0.0] for _ in cpaths],
                       "means": [[0.2, 0.2, 0.2]] * len(cpaths),
                       "window": 9, "strength": 100, "channels": True},
                      persist=True)
    pump(app, 200)
    stored = edits.load_deflicker(dev._edit_key, cpaths) or {}
    check("分通道偏移落盘也守 ±3 EV",
          stored.get("offsets")
          and all(abs(x) <= flicker.MAX_EV + 1e-9 for o in stored["offsets"]
                  for x in o), str(stored.get("offsets") and stored["offsets"][0]))
    pb = dev._preview_params(0)
    check("内存里的越界修正套用前夹住",
          len(pb.get("chan_ev") or []) == 3
          and pb["chan_ev"][0] == flicker.MAX_EV
          and pb["chan_ev"][1] == -flicker.MAX_EV, str(pb.get("chan_ev")))
    dlg3.close()
    dev.clear_deflicker()
    pump(app, 200)


def test_keyframes(app, dev):
    """序列关键帧：启用即记录、改参数自动记录、导航取插值、开关与持久化。"""
    d = make_series(12, os.path.join(TMP, "kf_series"))
    paths = [os.path.join(d, f"wb_{i:04d}.jpg") for i in range(12)]
    dev.set_media_list(paths)
    pump(app, 500)
    check("关键帧入口可用", dev._keyframe_capable()
          and dev.cb_keyframes.isEnabled() and dev.btn_kf_record.isEnabled())
    check("初始没有关键帧", not dev._keyframes and not dev._keyframes_on)

    dev.store.set("exposure", 0.0)
    dev.cb_keyframes.setChecked(True)
    pump(app, 300)
    check("启用即记录当前帧", dev._keyframes_on
          and seq.indices(dev._keyframes) == [0],
          str(seq.indices(dev._keyframes)))
    check("时间轴有关键帧标记", dev.sl_time.marks == [0],
          str(dev.sl_time.marks))
    kf_path = edits.keyframes_path(dev._edit_key)
    check("关键帧自动落盘", os.path.exists(kf_path))

    dev.select_index(10)
    pump(app, 400)
    dev.store.set("exposure", 3.0)
    pump(app, 900)                      # 500ms 防抖 + 余量
    check("改参数自动记录当前帧",
          seq.indices(dev._keyframes) == [0, 10],
          str(seq.indices(dev._keyframes)))
    check("两端关键帧参数正确",
          abs(dev._preview_params(0)["exposure"]) < 1e-6
          and abs(dev._preview_params(10)["exposure"] - 3.0) < 1e-6)
    check("中间帧插值", abs(dev._preview_params(5)["exposure"] - 1.5) < 1e-6,
          f"{dev._preview_params(5)['exposure']:+.3f}")
    at = dev._batch_params()
    check("批量导出逐帧插值", at is not None
          and abs(at(paths[0])["exposure"]) < 1e-6
          and abs(at(paths[5])["exposure"] - 1.5) < 1e-6
          and abs(at(paths[10])["exposure"] - 3.0) < 1e-6,
          "" if at is None else f"{at(paths[5])['exposure']:+.2f}")

    dev.select_index(5)
    pump(app, 500)
    check("导航到中间帧面板同步",
          abs(dev.store.value("exposure") - 1.5) < 1e-6,
          f"{dev.store.value('exposure'):+.3f}")
    check("关键帧状态行", "2 个关键帧" in dev.lb_keyframes.text()
          and "第 6 帧" in dev.lb_keyframes.text(),
          dev.lb_keyframes.text())

    dev.cb_keyframes.setChecked(False)
    pump(app, 400)
    check("关闭插值后全序列用当前参数",
          abs(dev._preview_params(0)["exposure"] - 1.5) < 1e-6
          and abs(dev._preview_params(10)["exposure"] - 1.5) < 1e-6)
    base = edits.load(dev._edit_key)
    check("关闭时当前参数落为基准", base is not None
          and abs(base["exposure"] - 1.5) < 1e-6,
          str(base and base["exposure"]))
    kb = edits.load_keyframes(dev._edit_key, paths)
    check("关闭状态也写进旁车", kb is not None and kb["on"] is False
          and [k["index"] for k in kb["keyframes"]] == [0, 10],
          str(kb and kb["on"]))

    dev.cb_keyframes.setChecked(True)
    pump(app, 400)
    check("重新启用恢复两端参数",
          abs(dev._preview_params(0)["exposure"]) < 1e-6
          and abs(dev._preview_params(10)["exposure"] - 3.0) < 1e-6)

    # 模拟重启：清掉内存状态，重走项目切换
    dev._keyframes = []
    dev._keyframes_on = False
    dev._kf_sig = None
    dev._edit_key = None
    dev._switch_edits(paths[0])
    pump(app, 500)
    check("重开序列恢复关键帧", dev._keyframes_on
          and seq.indices(dev._keyframes) == [0, 10],
          str(seq.indices(dev._keyframes)))
    check("重开序列按当前帧取参数",
          abs(dev.store.value("exposure") - 1.5) < 1e-6,
          f"{dev.store.value('exposure'):+.3f}")

    # A3：参数曲线也随关键帧过渡。日夜渐变里曲线是主力（提暗部、压高光），
    # 如果它只跟着全局走，关键帧插出来的就是"曝光在动、色调不变"的假过渡。
    cur_a = [[0, 30], [128, 150], [255, 255]]     # 暗部抬起
    cur_b = [[0, 0], [128, 90], [255, 220]]       # 整体压暗
    dev.select_index(0)
    pump(app, 400)
    dev.store.set("curve_rgb", cur_a)
    pump(app, 700)                                # 500ms 防抖：写进这一帧的关键帧
    dev.select_index(10)
    pump(app, 400)
    dev.store.set("curve_rgb", cur_b)
    pump(app, 700)

    def _mean_lut(c):
        return float(gc.pchip_lut(c, 256).mean())

    check("曲线也记进两端关键帧",
          seq.at(dev._keyframes, 0)["params"]["curve_rgb"] == cur_a
          and seq.at(dev._keyframes, 10)["params"]["curve_rgb"] == cur_b,
          f"{_mean_lut(cur_a):.1f} / {_mean_lut(cur_b):.1f}")
    mid = dev._preview_params(5)["curve_rgb"]
    check("中间帧的曲线是两端重采样出来的，不是硬切",
          min(_mean_lut(cur_a), _mean_lut(cur_b)) < _mean_lut(mid)
          < max(_mean_lut(cur_a), _mean_lut(cur_b)),
          f"{_mean_lut(cur_a):.1f} → {_mean_lut(mid):.1f} → {_mean_lut(cur_b):.1f}")
    check("端点取回自己那条曲线，没被插值污染",
          dev._preview_params(0)["curve_rgb"] == cur_a
          and dev._preview_params(10)["curve_rgb"] == cur_b)
    bat = dev._batch_params()
    b0, b5, b10 = (_mean_lut(bat(paths[i])["curve_rgb"]) for i in (0, 5, 10))
    check("批量导出逐帧取自己的曲线",
          abs(b0 - _mean_lut(cur_a)) < 1e-6 and abs(b10 - _mean_lut(cur_b)) < 1e-6
          and min(b0, b10) < b5 < max(b0, b10), f"{b0:.1f} / {b5:.1f} / {b10:.1f}")
    kb = edits.load_keyframes(dev._edit_key, paths)
    check("曲线跟着关键帧落盘",
          kb is not None
          and seq.at(kb["keyframes"], 0)["params"]["curve_rgb"] == cur_a
          and seq.at(kb["keyframes"], 10)["params"]["curve_rgb"] == cur_b,
          str(kb and [k["index"] for k in kb["keyframes"]]))

    # 删除与清空
    dev.select_index(0)
    pump(app, 400)
    dev.remove_keyframe()
    pump(app, 300)
    check("删除当前关键帧", seq.indices(dev._keyframes) == [10],
          str(seq.indices(dev._keyframes)))
    check("删掉首个关键帧后取最近值",
          abs(dev._preview_params(0)["exposure"] - 3.0) < 1e-6)
    dev.clear_keyframes()
    pump(app, 300)
    check("清空关键帧并停用", not dev._keyframes and not dev._keyframes_on
          and edits.load_keyframes(dev._edit_key, paths) is None)
    check("清空后沿用当前参数",
          abs(dev._preview_params(0)["exposure"] - 3.0) < 1e-6)


def _wait_luma(app, curve, n, timeout=30):
    """等后台把逐帧亮度测完（信号要主线程转圈才收得到）。"""
    t0 = time.time()
    while len(curve.values) < n and time.time() - t0 < timeout:
        pump(app, 100)
    return len(curve.values) >= n


def _drag_curve(app, curve, i, dy, alt=False):
    """在第 i 帧按住往上拖 dy 像素再松手，返回这样拖对应的 EV 增量。

    几何公式和 `LumaCurve` 自己的一套常数（EV_FULL / PAD）绑在一起，
    省得测试和界面各写一遍、改了那边忘了改这边。
    """
    n = max(2, len(curve.values))
    r = curve.rect().adjusted(0, 0, 0, -1)
    h = max(1.0, r.height() - 2 * curve.PAD)
    x = r.left() + i / (n - 1) * max(1, r.width() - 1)
    y0 = r.bottom() - curve.PAD - 1.0
    step = curve.EV_FULL / h / (5.0 if alt else 1.0)
    btn = QtCore.Qt.MouseButton.LeftButton
    none = QtCore.Qt.MouseButton.NoButton
    mod = (QtCore.Qt.KeyboardModifier.AltModifier if alt
           else QtCore.Qt.KeyboardModifier.NoModifier)
    for typ, y, b, bs in (
            (QtCore.QEvent.Type.MouseButtonPress, y0, btn, btn),
            (QtCore.QEvent.Type.MouseMove, y0 - dy, none, btn),
            (QtCore.QEvent.Type.MouseButtonRelease, y0 - dy, btn, none)):
        ev = QtGui.QMouseEvent(typ, QtCore.QPointF(x, y), b, bs, mod)
        if typ is QtCore.QEvent.Type.MouseButtonPress:
            curve.mousePressEvent(ev)
        elif typ is QtCore.QEvent.Type.MouseMove:
            curve.mouseMoveEvent(ev)
        else:
            curve.mouseReleaseEvent(ev)
    pump(app, 60)
    return dy * step


def _argb(curve):
    """把曲线当前画成什么样取下来（ARGB32，好逐像素比）。"""
    return curve.grab().toImage().convertToFormat(
        QtGui.QImage.Format.Format_ARGB32)


def _diff_pixels(a, b):
    """两张一样大的图差了多少个像素：第二条线画没画出来就用这个判。"""
    ba, bb = bytes(a.constBits()), bytes(b.constBits())
    return sum(1 for x, y in zip(ba, bb) if x != y) // 4


def test_curve_drag(app, wb, dev):
    """拖胶片条上的亮度曲线写曝光：自动开插值、首尾下锚、一条坡、一步撤销。"""
    d = make_series(12, os.path.join(TMP, "curve_series"))
    paths = [os.path.join(d, f"wb_{i:04d}.jpg") for i in range(12)]
    dev.set_media_list(paths)
    dev.set_sub_mode("显影")
    curve = dev.browser.curve
    check("序列显影页测出逐帧亮度", _wait_luma(app, curve, 12),
          f"{len(curve.values)} 个值")
    check("能逐帧调色才让拖曲线",
          curve.editable and dev._keyframe_capable()
          and curve.toolTip().startswith("逐帧亮度"),
          curve.toolTip().splitlines()[0] if curve.toolTip() else "没有悬停说明")

    # 只按下不拖：跳帧可以，凭空生出关键帧不行
    _drag_curve(app, curve, 3, 0.0)
    pump(app, 700)
    check("点一下只跳帧，不改参数也不生关键帧",
          dev.browser.index == 3 and not dev._keyframes
          and not dev._keyframes_on and abs(dev.store.value("exposure")) < 1e-6,
          f"{seq.indices(dev._keyframes)}")

    ev = _drag_curve(app, curve, 6, 18.5)
    pump(app, 900)                      # 500ms 防抖把这次编辑落成关键帧
    check("拖第 6 帧拖出满量程的一半", 1.9 < ev < 2.1, f"{ev:+.2f} EV")
    check("拖动自动开启逐帧插值", dev._keyframes_on)
    check("首尾自动下锚点，中间是拖出来的峰",
          seq.indices(dev._keyframes) == [0, 6, 11],
          str(seq.indices(dev._keyframes)))
    check("拖到的那帧按量变亮",
          abs(dev._preview_params(6)["exposure"] - 2.0) < 0.05,
          f"{dev._preview_params(6)['exposure']:+.2f}")
    check("两端保持不动",
          abs(dev._preview_params(0)["exposure"]) < 1e-6
          and abs(dev._preview_params(11)["exposure"]) < 1e-6)
    check("中间帧连成坡", abs(dev._preview_params(3)["exposure"] - 1.0) < 0.05,
          f"{dev._preview_params(3)['exposure']:+.2f}")
    at = dev._batch_params()
    check("批量导出走同一条坡", at is not None
          and abs(at(paths[3])["exposure"] - 1.0) < 0.05
          and abs(at(paths[11])["exposure"]) < 1e-6)
    kb = edits.load_keyframes(dev._edit_key, paths)
    check("拖动写进关键帧旁车", kb is not None and kb["on"]
          and [k["index"] for k in kb["keyframes"]] == [0, 6, 11]
          and abs(kb["keyframes"][1]["params"]["exposure"] - 2.0) < 0.05,
          str(kb and [k["index"] for k in kb["keyframes"]]))
    check("曲线拿到逐帧曝光", len(curve.evs) == 12
          and abs(curve.evs[6] - 2.0) < 0.05
          and abs(curve.evs[0]) < 1e-6, f"{curve.evs[6]:+.2f}")
    check("校正后的线跟着亮",
          curve.corrected[6] is not None and curve.values[6] is not None
          and curve.corrected[6] > curve.values[6]
          and abs(curve.corrected[0] - curve.values[0]) < 1e-6)
    # 像素级锁第二条线：只把偏移撤掉再画一次，差出来的像素就是那条线本身
    with_ghost = _argb(curve)
    curve.set_offsets([])
    pump(app, 50)
    plain = _argb(curve)
    dev._update_curve_corrected()
    pump(app, 50)
    n_diff = _diff_pixels(with_ghost, plain)
    check("第二条线真的画在曲线上",
          curve._ghost() is not None and n_diff > 50, f"{n_diff} 个像素不同")
    _path, size = shoot(wb, "亮度曲线拖拽")
    check("拖出坡之后有截图", size > 8000, f"{size} 字节")

    # Alt 精调：五分之一档，拖同样的像素只动一点点
    e4 = float(dev._preview_params(4)["exposure"])
    fine = _drag_curve(app, curve, 4, 5.0, alt=True)
    pump(app, 900)
    check("按住 Alt 是精调（1/5 档）", 0.08 < fine < 0.14, f"{fine:+.3f} EV")
    check("精调叠在这一帧原本的坡上",
          abs(dev.store.value("exposure") - (e4 + fine)) < 0.02,
          f"{dev.store.value('exposure'):+.3f} vs {e4 + fine:+.3f}")
    check("第 4 帧也成为关键帧", 4 in seq.indices(dev._keyframes),
          str(seq.indices(dev._keyframes)))

    dev.undo()
    pump(app, 900)
    check("一次拖动算一步撤销", abs(dev.store.value("exposure") - e4) < 1e-6,
          f"{dev.store.value('exposure'):+.3f} vs {e4:+.3f}")
    check("撤销后第 4 帧回到原来的坡上",
          abs(dev._preview_params(4)["exposure"] - e4) < 0.02,
          f"{dev._preview_params(4)['exposure']:+.3f}")
    check("坡顶没被牵连", abs(dev._preview_params(6)["exposure"] - 2.0) < 0.02)

    # 只有一张图时不能拖：那时没有"逐帧"可言
    dev.set_media_list(paths[:1])
    pump(app, 400)
    check("单张图不给拖（没有逐帧这回事）",
          not dev._keyframe_capable() and not curve.editable)
    dev.set_media_list(paths)
    pump(app, 300)
    check("回到序列又能拖", curve.editable)

    # 只读那张曲线还得能点着跳帧（老行为不能丢）
    probe = filmstrip.Browser()
    probe.set_mode("strip")
    probe.set_items(paths)
    ok = _wait_luma(app, probe.curve, 12)
    got = []
    probe.curve.seeked.connect(got.append)
    if ok:
        r = probe.curve.rect().adjusted(0, 0, 0, -1)
        x = r.left() + 5 / (len(probe.curve.values) - 1) * max(1, r.width() - 1)
        probe.curve.mousePressEvent(QtGui.QMouseEvent(
            QtCore.QEvent.Type.MouseButtonPress,
            QtCore.QPointF(x, r.center().y()),
            QtCore.Qt.MouseButton.LeftButton, QtCore.Qt.MouseButton.LeftButton,
            QtCore.Qt.KeyboardModifier.NoModifier))
        probe.curve.mouseReleaseEvent(QtGui.QMouseEvent(
            QtCore.QEvent.Type.MouseButtonRelease,
            QtCore.QPointF(x, r.center().y()),
            QtCore.Qt.MouseButton.LeftButton, QtCore.Qt.MouseButton.NoButton,
            QtCore.Qt.KeyboardModifier.NoModifier))
        pump(app, 60)
    check("只读时点曲线还是跳帧",
          ok and not probe.curve.editable and got == [5], str(got))
    probe.shutdown()
    probe.deleteLater()
    dev.set_sub_mode("图库")
    pump(app, 200)
    dev.clear_keyframes()
    pump(app, 300)


def _sheet_bands(k, cols, cell, w0, h0):
    """联系表第 k 格里"原图块 / 调色后块"的取图范围。

    几何照抄 exporter.contact_sheet，测试不重复写常数（gap/div/label 改了就错位）。
    """
    gap, div, lab = exporter.SHEET_GAP, exporter.SHEET_DIV, exporter.SHEET_LABEL
    ch = max(24, int(round(cell * h0 / max(w0, 1))))
    cw = 2 * cell + div
    r, c = divmod(k, cols)
    y = gap + r * (ch + lab + gap)
    x = gap + c * (cw + gap)
    box = slice(y, y + ch)
    return box, slice(x, x + cell), slice(x + cell + div, x + cell + div + cell)


def test_sheet(app, wb, dev):
    """联系表：每格左原图右调色后，抽帧跟着体检走，参数逐帧插值也进表。"""
    # ---- 纯几何：黑白对拼出来就该左暗右亮、中间一条白线、下面有帧号 ----
    black = np.zeros((60, 120, 3), np.float32)
    white = np.ones((60, 120, 3), np.float32)
    pairs = [(black, white)] * 3
    png = exporter.contact_sheet(pairs, ["#0001", "#0003", "#0007"],
                                 cell=100, cols=2)
    y, xb, xa = _sheet_bands(0, 2, 100, 120, 60)
    check("联系表左半边是原图", png[y, xb].mean() < 3,
          f"{png[y, xb].mean():.1f}")
    check("联系表右半边是调色后", png[y, xa].mean() > 250,
          f"{png[y, xa].mean():.1f}")
    check("中间一条白线分开",
          png[y, 108:110].min() > 250, str(png[y, 108:110].min()))
    check("格子下面写了帧号", png[58:76, 8:420].sum() > 0,
          str(int(png[58:76, 8:420].sum())))
    check("三格两列排两行", png.shape == (160, 428, 3), str(png.shape))
    wide = exporter.contact_sheet(pairs, [], cell=100, cols=9)
    check("列数不会多过帧数", wide.shape == (84, 638, 3), str(wide.shape))
    check("没有格子就没有表", exporter.contact_sheet([], []) is None)
    s = exporter.sample_indices(12, 6)
    check("抽帧首尾都取到", s[0] == 0 and s[-1] == 11 and len(set(s)) == 6,
          str(s))
    check("要得比帧数多就全给", exporter.sample_indices(3, 24) == [0, 1, 2]
          and exporter.sample_indices(1, 6) == [0]
          and exporter.sample_indices(0, 6) == [])

    # ---- 端到端：真的走一遍导出 ----
    d = make_series(12, os.path.join(TMP, "sheet_series"))
    paths = [os.path.join(d, f"wb_{i:04d}.jpg") for i in range(12)]
    dev.set_media_list(paths)
    dev.set_sub_mode("显影")
    pump(app, 600)
    check("联系表拿的是播放序列", len(dev._play_items) == 12,
          f"{len(dev._play_items)} 帧")
    check("联系表控件在导出面板上", dev.btn_sheet.isEnabled()
          and "联系表" in dev.btn_sheet.toolTip())
    wb.set_module("调色台")
    pump(app, 300)
    dev.store.reset_all()
    dev.cb_keyframes.setChecked(False)
    pump(app, 700)

    out_dir = os.path.join(TMP, "wb_sheet")
    shutil.rmtree(out_dir, ignore_errors=True)
    os.makedirs(out_dir)
    box = {"p": os.path.join(out_dir, "sheet_a.png")}
    real_dlg = QtWidgets.QFileDialog.getSaveFileName
    QtWidgets.QFileDialog.getSaveFileName = staticmethod(
        lambda *a, **k: (box["p"], ""))      # 不桩掉就是模态框，pump 转不动它
    real_warn = dev.warn
    warns = []
    dev.warn = lambda msg, *a, **k: warns.append(msg)
    dev.ex_sheet_n.setCurrentIndex(0)       # 6 帧
    dev.ex_sheet_cols.setCurrentIndex(0)    # 2 列
    try:
        # 一张图没有"整段"可看
        dev.set_media_list(paths[:1])
        pump(app, 400)
        dev.export_sheet()
        check("单张图说不做联系表", any("至少两张" in w for w in warns),
              str(warns))
        dev.set_media_list(paths)
        pump(app, 600)

        # 抽帧的帧号按播放序列算，体检排除的帧不该占位
        started = []
        real_start = dev.start_export
        dev.start_export = lambda job, cb: started.append(job)
        dev.set_qa_excluded([paths[0], paths[5]])
        box["p"] = os.path.join(out_dir, "never.png")
        dev.export_sheet()
        dev.start_export = real_start
        dev.clear_qa()
        job = started[-1]
        nums = [i for i, _p in job.picks]
        check("体检排除的帧不进联系表",
              len(nums) == 6 and 0 not in nums and 5 not in nums
              and nums[-1] == 11 and nums == sorted(nums), str(nums))
        check("联系表参数传到任务里",
              job.cell == developer.SHEET_CELL
              and job.cols == dev.ex_sheet_cols.currentData()
              and job.dst.endswith(".png"), f"{job.cell}/{job.cols}")

        def _run(dst):
            box["p"] = dst
            dev.export_sheet()
            check("导出期间联系表按钮禁用", dev.btn_sheet.isEnabled() is False)
            ok = wait_job(app, wb, 90)
            pump(app, 300)
            return ok

        # A：默认参数（前后应当一样）
        a_dst = os.path.join(out_dir, "sheet_a.png")
        check("默认参数的联系表跑完", _run(a_dst))
        # B：整体提亮（只动右半边）
        dev.store.set("exposure", 2.0)
        pump(app, 800)
        b_dst = os.path.join(out_dir, "sheet_b.png")
        check("提亮后的联系表跑完", _run(b_dst))

        def _load(p):
            im = cv2.imread(p)
            assert im is not None, p
            return cv2.cvtColor(im, cv2.COLOR_BGR2RGB).astype(np.float32) / 255.0

        A, B = _load(a_dst), _load(b_dst)
        y, xb, xa = _sheet_bands(0, 2, developer.SHEET_CELL, 400, 300)
        check("联系表有两块对照内容", A.shape == B.shape and A.shape[0] > 300,
              str(A.shape))
        check("原图半边不受当前参数影响",
              abs(A[y, xb] - B[y, xb]).mean() < 0.004,
              f"{abs(A[y, xb] - B[y, xb]).mean():.4f}")
        check("调色半边跟着参数变亮",
              B[y, xa].mean() > B[y, xb].mean() + 0.05
              and abs(A[y, xa] - B[y, xa]).mean() > 0.05,
              f"{B[y, xb].mean():.3f} → {B[y, xa].mean():.3f}")
        check("默认参数下前后一样（原图没被调两遍）",
              abs(A[y, xb] - A[y, xa]).mean() < 0.01,
              f"{abs(A[y, xb] - A[y, xa]).mean():.4f}")
        check("联系表状态行", "联系表" in wb.lb_status.text(),
              wb.lb_status.text())
        dev.store.reset_all()
        pump(app, 800)

        # C：两端下锚，每格该用自己那一帧的参数
        dev.cb_keyframes.setChecked(True)
        pump(app, 400)
        dev.select_index(11)
        pump(app, 500)
        dev.store.set("exposure", 1.0)
        pump(app, 900)
        c_dst = os.path.join(out_dir, "sheet_c.png")
        check("逐帧参数的联系表跑完", _run(c_dst))
        C = _load(c_dst)
        means = []
        for k in range(6):
            yy, _b, xx = _sheet_bands(k, 2, developer.SHEET_CELL, 400, 300)
            means.append(float(C[yy, xx].mean()))
        check("每格用那一帧自己的参数",
              means[-1] - means[0] > 0.03
              and all(b >= a - 0.004 for a, b in zip(means, means[1:])),
              " → ".join(f"{m:.3f}" for m in means))
    finally:
        dev.start_export = real_start
        dev.warn = real_warn
        QtWidgets.QFileDialog.getSaveFileName = real_dlg
        dev.cb_keyframes.setChecked(False)
        dev.clear_keyframes()
        dev.store.reset_all()
        pump(app, 600)
    dev.set_sub_mode("导出")      # 截图要拍到新加的那一节，得站在导出页
    pump(app, 400)
    check("导出页看得见联系表这一节", dev.btn_sheet.isVisible()
          and dev.ex_sheet_n.isVisible() and dev.ex_sheet_cols.isVisible())
    path, size = shoot(wb, "联系表面板")
    check("联系表面板截图", size > 10000, f"{size // 1024}KB {path}")
    dev.set_sub_mode("显影")
    pump(app, 200)


def test_restore(app, wb):
    """新开一个工作台：恢复上次项目与上次工具，起始页不再挡着。"""
    path, kind = wb.project_path, wb.project_kind
    prefs.put("shell/last_module", "对齐")
    wb2 = workbench.Workbench(MODULES)
    wb2.show()
    pump(app, 1200)
    check("启动恢复上次项目", wb2.project_path == os.path.normpath(path)
          and wb2.material.kind == kind,
          f"{wb2.project_kind} {wb2.project_path}")
    check("启动恢复上次工具", wb2._active is not None
          and wb2._active.name == "对齐", str(wb2._active and wb2._active.name))
    wb2.close()
    pump(app, 300)

    # 带素材启动（restore=False）不能先载入上次项目，否则大序列白读一遍
    wb3 = workbench.Workbench(MODULES, restore=False)
    check("命令行走 restore=False 不恢复上次",
          wb3.project_path == "" and wb3.material.is_empty,
          f"{wb3.project_path} / {wb3.material.label()}")
    wb3.close()
    pump(app, 200)

    # 窗口弧线同样从设置里恢复：故意改成直角再新建一个工作台
    wb._settings().setValue("shell/corner", 1)
    wb4 = workbench.Workbench(MODULES, restore=False)
    check("启动恢复窗口弧线", theme.CORNER == 1, str(theme.CORNER))
    wb4.close()
    pump(app, 200)
    wb._settings().setValue("shell/corner", 2)
    theme.set_corner_mode(2)

    # 字号档同样从设置里恢复：上一个窗口调到特大，新窗口不能回到标准
    wb._settings().setValue("shell/font", "宽松")
    wb5 = workbench.Workbench(MODULES, restore=False)
    check("启动恢复字号档", theme.FONT == "宽松" and theme.fs(15) == 17
          and "font-size: 17px" in theme.QSS,
          f"{theme.FONT} 正文 {theme.fs(15)}px")
    wb5.close()
    pump(app, 200)
    wb._settings().setValue("shell/font", "标准")
    theme.set_font(app, "标准")


class _HoldJob(jobs.Job):
    """占住任务槽直到取消。

    不能用真实合成任务来测互斥：14 帧 0.1 秒的片子可能在断言前就跑完了，
    这条检查就会随机失败。
    """

    def run(self):
        while not self.stopping:
            time.sleep(0.02)
        self.done.emit(False, "cancelled")


def test_one_job_at_a_time(app, wb):
    QtWidgets.QMessageBox.information = staticmethod(lambda *a, **k: None)
    first = _HoldJob([])
    check("任务槽可启动", wb.start_job(first, lambda *a: None))
    ok2 = wb.start_job(jobs.Job([]), lambda *a: None)
    check("同时只允许一个任务", ok2 is False and wb._job is first)
    first.cancel()
    wait_job(app, wb)


def test_job_cancel(app, wb):
    """任务层：取消要停掉 ffmpeg 并删掉不可播放的半成品。"""
    ff = find_ffmpeg()
    dst = os.path.join(TMP, "wb_partial.mp4")
    if os.path.exists(dst):
        os.remove(dst)
    cmd = [ff, "-y", "-hide_banner", "-loglevel", "error", "-nostats",
           "-progress", "pipe:1", "-f", "lavfi",
           "-i", "testsrc=duration=120:size=1920x1080:rate=30",
           "-c:v", "libx264", "-preset", "veryfast", dst]
    box = {}
    job = jobs.Job([("1920x1080", cmd, jobs.Meter("time", 120.0), dst, None)])
    wb.start_job(job, lambda ok, res: box.update(ok=ok, res=res))
    t0 = time.time()
    while wb.prog.value() == 0 and time.time() - t0 < 60:
        pump(app, 50)
    check("长任务进度在推进", 0 < wb.prog.value() < 1000, str(wb.prog.value()))
    wb.cancel_job()
    t0 = time.time()
    while job.isRunning() and time.time() - t0 < 30:
        pump(app, 50)
    pump(app, 300)
    check("取消后 ffmpeg 已停", not job.isRunning())
    check("取消不保留半成品", not os.path.exists(dst))
    check("取消回传 cancelled", box.get("res") == "cancelled" and box.get("ok") is False,
          str(box))


def test_cancel_batch(app, wb, dev):
    """批量导出：进度推进 -> 取消 -> 任务停 + 按钮复位。"""
    wb.set_module(dev.name)      # 导出时用户就在调色台，状态栏才归它说话
    items = sorted(os.path.join(SRC_BIG, n) for n in os.listdir(SRC_BIG)
                   if n.endswith(".jpg"))[:12]
    check("用真实序列测批量导出", len(items) == 12, f"{len(items)} 张")
    out_dir = os.path.join(TMP, "wb_batch")
    os.makedirs(out_dir, exist_ok=True)
    for f in os.listdir(out_dir):
        os.remove(os.path.join(out_dir, f))
    dev.media_items = list(items)
    dev.ex_q.setValue(90)
    # 跳过文件对话框：直接调用与按钮等价的内部流程
    job = developer.BatchExport(items, out_dir, dev.store.p, 90)
    dev.start_export(job, lambda ok, res: dev._batch_done(ok, res, out_dir))
    t0 = time.time()
    # 全分辨率一张要 5984x3959 解码 + 调色 + 编码，进度是逐张发的，等一下首张
    while wb.prog.value() == 0 and time.time() - t0 < 60:
        pump(app, 50)
    check("批量导出推进度", wb.prog.value() > 0, str(wb.prog.value()))
    check("导出期间按钮禁用", dev.btn_batch.isEnabled() is False)
    wb.cancel_job()
    check("取消跑完", wait_job(app, wb, 60))
    n = len([f for f in os.listdir(out_dir) if f.endswith(".jpg")])
    check("取消后停止写盘", n < len(items), f"已写 {n}/{len(items)}")
    check("取消后按钮复位", dev.btn_batch.isEnabled() and not wb.prog.isVisible())
    check("中断有说明", "中断" in wb.lb_status.text(), wb.lb_status.text())

    # 再跑完一次，确认成功路径与产物
    dev.store.load_preset("落日暖调")
    for f in os.listdir(out_dir):
        os.remove(os.path.join(out_dir, f))
    job = developer.BatchExport(items, out_dir, dev.store.p, 90)
    dev.start_export(job, lambda ok, res: dev._batch_done(ok, res, out_dir))
    check("批量导出跑完", wait_job(app, wb, 180))
    outs = [f for f in os.listdir(out_dir) if f.endswith(".jpg")]
    check("全部导出成功", len(outs) == len(items)
          and all(os.path.getsize(os.path.join(out_dir, f)) > 1000 for f in outs),
          f"{len(outs)}/{len(items)}")
    check("完成后进度条收起", not wb.prog.isVisible() and not wb.btn_cancel.isVisible()
          and "批量导出" in wb.lb_status.text(), wb.lb_status.text())


def main():
    # 模块会把上次目录/参数写进 QSettings；测试必须落到临时 INI，
    # 否则既会污染用户的真实偏好，也会让测试读到上次跑剩的值
    ini = os.path.join(TMP, "settings")
    shutil.rmtree(ini, ignore_errors=True)
    QtCore.QSettings.setDefaultFormat(QtCore.QSettings.Format.IniFormat)
    QtCore.QSettings.setPath(QtCore.QSettings.Format.IniFormat,
                             QtCore.QSettings.Scope.UserScope, ini)
    # 缓存与插件走测试目录：回归不该往用户真实目录里写缩略图或建插件夹
    prefs.put("cache/dir", os.path.join(TMP, "wb_cache"))
    prefs.put("plugins/dir", os.path.join(TMP, "wb_plugins"))
    prefs.put("edits/dir", os.path.join(TMP, "wb_edits"))
    shutil.rmtree(os.path.join(TMP, "wb_edits"), ignore_errors=True)
    app = QtWidgets.QApplication(sys.argv)
    theme.apply(app)
    test_meter()
    test_thumb_cache()
    test_geometry()
    wb, asm, dev = test_shell(app)
    video = test_pipeline(app, wb, asm, dev)
    test_settings(app, wb, dev)
    test_drop(app, wb, video)
    test_reopen(app, wb, video)
    test_import_async(app, wb, video)
    test_one_job_at_a_time(app, wb)
    test_job_cancel(app, wb)
    test_cancel_batch(app, wb, dev)
    test_edits(app, dev)
    test_deflicker(app, dev)
    test_keyframes(app, dev)
    test_curve_drag(app, wb, dev)
    test_sheet(app, wb, dev)
    test_restore(app, wb)
    wb.close()
    pump(app, 200)
    test_selftest(os.path.abspath(SERIES))
    print("\n" + ("全部通过" if not FAILS else f"失败 {len(FAILS)}: {FAILS}"))
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
