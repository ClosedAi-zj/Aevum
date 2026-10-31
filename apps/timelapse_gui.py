# -*- coding: utf-8 -*-
"""延时视频合成工具（Tk 界面）入口。双击 延时视频.bat 即可。"""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "acceptance"))

from reference.gui.timelapse import TimelapseApp  # noqa: E402

if __name__ == "__main__":
    TimelapseApp().mainloop()
