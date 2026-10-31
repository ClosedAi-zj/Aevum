# -*- coding: utf-8 -*-
r"""打 onedir 版 exe：python apps/build_exe.py（或双击 打包.bat）。

产物是 dist\延时工作台\ 整个文件夹，exe 在里头；拷给别人要连文件夹一起拷，
因为 Qt 的 dll 和 imageio-ffmpeg 自带的那个 ffmpeg.exe 都在同级 _internal 里。

不出 onefile：那种单文件 exe 每次启动都要把几百 MB 解压到临时目录，
首开十几秒，而且自解压行为容易被 Windows Defender 拦，对"以后要开源"更不友好。
"""
import os
import sys

from PyInstaller.__main__ import run

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# Python 参考实现搬进了验收目录，包的父目录不再是仓库根。
REFERENCE_ROOT = os.path.join(ROOT, "acceptance")
NAME = "延时工作台"

ARGS = [
    "--noconfirm", "--clean",
    "--windowed",                     # 不带控制台；打包版自检请用 --check 弹窗
    "--name", NAME,
    "--icon", os.path.join(ROOT, "assets", "app.ico"),   # 文件图标，先跑 apps/make_icon.py
    "--distpath", os.path.join(ROOT, "dist"),
    "--workpath", os.path.join(ROOT, "build", "work"),
    "--specpath", os.path.join(ROOT, "build"),
    "--paths", REFERENCE_ROOT,        # reference 是 acceptance/ 下的包，分析器默认找不到
    # 标题栏与任务栏用的是窗口图标，得把 png 一起打进去（目标目录保持 assets/ 相对路径）
    "--add-data", os.path.join(ROOT, "assets", "app.png") + os.pathsep + "assets",
    # 随包开源字体：Inter + Noto Sans SC，缺了会退回系统雅黑
    "--add-data", os.path.join(ROOT, "assets", "fonts") + os.pathsep + "assets/fonts",
    # QSS 里复选标记用的矢量图标；缺了勾选框只剩一块靛蓝方块
    "--add-data", os.path.join(ROOT, "assets", "icons") + os.pathsep + "assets/icons",
    "--collect-binaries", "imageio_ffmpeg",   # ffmpeg 以数据文件形式发布，不收集就找不到
    os.path.join(ROOT, "apps", "workbench.py"),
]

# PyInstaller 顺着依赖图收进来、但我们永远走不到的躯干（相对 _internal 的 glob）：
#   cv2 的 opencv_videoio_ffmpeg —— cv2 自带的视频读写后端，本项目视频一律走
#     imageio_ffmpeg 的 ffmpeg；该 DLL 是延迟加载，删掉不影响 import cv2
#     （以后真要用 cv2.VideoCapture 再把这行去掉）。
#   Qt6Qml/Quick/Pdf/VirtualKeyboard —— 只被虚拟键盘与 qpdf 图片插件引用，
#     连插件一起删掉后这些 DLL 就没有任何加载方了。
# opengl32sw.dll（软件 OpenGL 兜底）特意留着：分发给没有独显/远程桌面的
# 机器时，缺了它整个界面黑屏，19MB 换可靠性。
PRUNE = [
    "cv2/opencv_videoio_ffmpeg*.dll",
    "PySide6/Qt6Qml*.dll",
    "PySide6/Qt6Quick*.dll",
    "PySide6/Qt6VirtualKeyboard*.dll",
    "PySide6/Qt6Pdf*.dll",
    "PySide6/plugins/platforminputcontexts/*",
    "PySide6/plugins/imageformats/qpdf.dll",
]


def prune(into):
    import glob
    saved = 0
    for pat in PRUNE:
        for p in glob.glob(os.path.join(into, *pat.split("/"))):
            saved += os.path.getsize(p)
            os.remove(p)
    return saved


def main():
    # 中文产物名要打印出来，GBK 控制台下 print 会直接抛异常
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    os.chdir(ROOT)
    run(ARGS)
    exe = os.path.join(ROOT, "dist", NAME, NAME + ".exe")
    if not os.path.exists(exe):
        print("打包失败：没有", exe)
        return 1
    saved = prune(os.path.join(ROOT, "dist", NAME, "_internal"))
    size = sum(os.path.getsize(os.path.join(d, f))
               for d, _, fs in os.walk(os.path.dirname(exe)) for f in fs)
    print(f"\n产物：{exe}\n体积：{size / 1024 / 1024:.0f} MB（整个文件夹，"
          f"剪掉 {saved / 1024 / 1024:.0f} MB 没用依赖）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
