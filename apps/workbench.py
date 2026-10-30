# -*- coding: utf-8 -*-
"""延时工作台（Qt 界面）入口：python apps/workbench.py [序列目录/图片/视频]

一个窗口装下所有工具：顶部一排工具标签，六个工具随时可切换，导入素材只是
自动把内容填进各页。调色台是默认工作区，对齐/合成/裁剪都是它的延伸。
传路径就把它当成当前项目，工具之间会自动接着用（合成写完的视频直接进调色台，
配乐写完的成片再交给裁剪）。
启动：双击 延时工作台.bat。
"""
import os
import sys

if not getattr(sys, "frozen", False):
    # 源码运行时把仓库根指进来；打包后模块已经在 exe 里了
    sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from PySide6 import QtGui, QtWidgets  # noqa: E402

from lapse import ff  # noqa: E402
from lapse.gui import theme  # noqa: E402
from lapse.gui.aligner import Aligner  # noqa: E402
from lapse.gui.assemble import Assemble  # noqa: E402
from lapse.gui.audiomix import AudioMix  # noqa: E402
from lapse.gui.cropper import Cropper  # noqa: E402
from lapse.gui.grader import developer  # noqa: E402
from lapse.gui.retime import Retime  # noqa: E402
from lapse.gui.workbench import Workbench  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# 注册顺序 = 顶部标签顺序：调色台是默认工作区，其余工具随时可用
MODULES = [developer.Developer, Aligner, Assemble, Cropper, AudioMix, Retime]


def make_app():
    """建 QApplication 并套主题 + 窗口图标（标题栏和任务栏用的是它，不是 exe 图标）。"""
    app = QtWidgets.QApplication(sys.argv)
    theme.apply(app)
    # 打包后资源在 _MEIPASS 下，相对路径 assets/ 与仓库里保持一致
    base = getattr(sys, "_MEIPASS", ROOT)
    icon = os.path.join(base, "assets", "app.png")
    if os.path.exists(icon):
        app.setWindowIcon(QtGui.QIcon(icon))
    return app


def selfcheck():
    """打包版没有控制台，依赖齐不齐只能弹窗看：延时工作台.exe --check"""
    import numpy
    from PySide6 import __version__ as qt

    rows = [f"Python  {sys.version.split()[0]}", f"PySide6  {qt}",
            f"numpy  {numpy.__version__}"]
    for mod in ("cv2", "rawpy", "imageio_ffmpeg"):
        try:
            __import__(mod)
            rows.append(f"{mod}  已就绪")
        except Exception as e:
            rows.append(f"{mod}  缺失：{e}")
    rows.append("ffmpeg  " + (ff.find_ffmpeg() or "未找到"))
    make_app()
    QtWidgets.QMessageBox.information(None, "延时工作台 · 自检", "\n".join(rows))


def open_path(w, path):
    """把命令行给的路径变成当前素材，并跳到合适的模块。"""
    return w.open_path(path)


def selftest(series):
    """打包版自检：模块、素材流水线、一次真实对齐预检（不写文件）。

    窗口版 exe 没有控制台，结果写到 %TEMP%\\lapse_selftest.txt，
    退出码 0/1 给脚本判断。预检抽满整段序列，确保走到 multiprocessing
    进程池——打包版最常出问题的就是这条路径。
    """
    import shutil
    import tempfile
    import traceback

    from PySide6 import QtCore

    report = os.path.join(tempfile.gettempdir(), "lapse_selftest.txt")
    rows = []
    ok = [True]

    def add(name, good, detail=""):
        rows.append(f"[{'PASS' if good else 'FAIL'}] {name}  {detail}")
        if not good:
            ok[0] = False

    try:
        # 自检不能改用户真实偏好：把 QSettings 落到临时 INI
        ini = os.path.join(tempfile.gettempdir(), "lapse_selftest_settings")
        shutil.rmtree(ini, ignore_errors=True)
        QtCore.QSettings.setDefaultFormat(QtCore.QSettings.Format.IniFormat)
        QtCore.QSettings.setPath(QtCore.QSettings.Format.IniFormat,
                                 QtCore.QSettings.Scope.UserScope, ini)

        import numpy
        from PySide6 import __version__ as qt
        add("Python", True, sys.version.split()[0])
        add("PySide6", True, qt)
        add("numpy", True, numpy.__version__)
        for mod in ("cv2", "rawpy", "imageio_ffmpeg"):
            try:
                __import__(mod)
                add(mod, True, "已就绪")
            except Exception as e:
                add(mod, False, str(e))

        app = make_app()
        w = Workbench(MODULES, restore=False)
        w.show()
        app.processEvents()        # 送达 Show：标题栏与窗口弧线都在那一步写
        names = list(w.mod_btns)
        add("六个工具", names == ["调色台", "对齐", "合成", "裁剪", "配乐", "运动"],
            " / ".join(names))
        add("默认工作区", w._active is w.module("调色台"),
            w.windowTitle())
        add("ffmpeg", bool(w.ff), w.ff or "未找到")
        qss = theme.QSS.lower()
        add("扁平主题", "gradient" not in qss and "box-shadow" not in qss,
            f"{len(theme.SKINS)} 套皮肤 · 当前 {theme.SKIN}")
        modes = {m for _label, m in theme.CORNER_MODES}
        add("窗口弧线", theme.CORNER in modes,
            f"{theme.corner_label(theme.CORNER)}  hr={theme.CORNER_HR}")

        add("序列目录存在", os.path.isdir(series), series)
        if series:
            w.set_module(open_path(w, series))
            add("素材流水线", w.material.kind == "series", w.material.label())
            alg = w.module("对齐")
            w.set_module("对齐")
            alg.ed_dir.setText(series)
            alg.detect()
            add("识别序列", bool(alg.files),
                f"{len(alg.files or [])} 张  {alg.lb_info.text()}")
            if alg.files:
                alg.sp_sample.setValue(200)   # 抽满，确保 > SMALL_BATCH
                alg.probe()
                job = getattr(alg, "_job", None)
                add("后台任务启动", job is not None and job.isRunning())
                if job is not None and job.wait(120000):
                    app.processEvents()       # 送达 done -> _show_summary
                    m = dict((n, v) for n, v, _t in alg.result.metric_items())
                    add("预检出结果", "px" in m.get("水平位移", ""),
                        f"{m.get('帧数')} 帧  dx {m.get('水平位移')}  "
                        f"dy {m.get('垂直位移')}")
                    add("预检不写文件", not os.path.isdir(alg.ed_out.text()))
                else:
                    if job is not None:
                        job.cancel()
                        job.wait(5000)
                    add("预检完成", False, "超时")
        w.close()
        app.processEvents()
    except Exception:
        add("自检异常", False, traceback.format_exc())

    with open(report, "w", encoding="utf-8") as f:
        f.write("\n".join(rows) + f"\n\n结果：{'通过' if ok[0] else '失败'}\n")
    return 0 if ok[0] else 1


def main():
    # 对齐模块用 multiprocessing.Pool；Windows 打包版必须 freeze_support，
    # 否则子进程会把 exe 当成脚本再跑一遍，弹出一串窗口
    import multiprocessing
    multiprocessing.freeze_support()
    if "--check" in sys.argv:
        selfcheck()
        return
    if "--selftest" in sys.argv:
        i = sys.argv.index("--selftest")
        series = sys.argv[i + 1] if i + 1 < len(sys.argv) else ""
        sys.exit(selftest(series))
    app = make_app()
    app.setApplicationName("Lapse Studio")
    args = [a for a in sys.argv[1:] if os.path.exists(a)]
    # 命令行给了素材就以它为准；别再先恢复上次项目，大序列会白读一遍
    w = Workbench(MODULES, restore=not args)
    w.show()
    if args:
        w.set_module(open_path(w, args[0]))
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
