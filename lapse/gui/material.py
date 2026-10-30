# -*- coding: utf-8 -*-
"""全软件共享的"当前素材"：序列目录 / 视频 / 图片三态。

模块之间不互相引用，只读这一个状态：对齐写完序列 -> 合成读序列 -> 合成写完
视频 -> 裁剪和调色台读视频。路径没变就不发信号，避免模块之间来回踢皮球。
"""
import os

from PySide6 import QtCore

from lapse import video

KIND_WORDS = {"video": "视频", "image": "图片", "series": "序列"}


class Material(QtCore.QObject):
    changed = QtCore.Signal(str, str)       # kind, path（kind 为空表示清空）

    def __init__(self, parent=None):
        super().__init__(parent)
        self.kind = ""
        self.path = ""
        self.series = None                  # video.scan_series 的结果

    # ------------------------------------------------------------------ 写入
    def set_series(self, d):
        return self._set("series", os.path.normpath(d))

    def set_video(self, p):
        return self._set("video", os.path.normpath(p))

    def set_image(self, p):
        return self._set("image", os.path.normpath(p))

    def clear(self):
        return self._set("", "")

    def reopen(self):
        """用户从「打开素材」再选一次同一个文件：状态没变也要真的重载。

        `_set` 的去重是给流水线用的（对齐写完给合成，合成不要再回踢对齐）；
        但从导入入口进来是用户主动要求的。胶片条里点图片不会改 Material，
        所以"看着还是那条视频、其实画面已经换成图片"时只能靠这里补发一次。
        """
        if self.kind or self.path:
            self.changed.emit(self.kind, self.path)
            return True
        return False

    def _set(self, kind, path):
        if self.kind == kind and self.path == path:
            return False
        self.kind, self.path = kind, path
        if kind != "series":
            self.series = None
        self.changed.emit(kind, path)
        return True

    # ------------------------------------------------------------------ 读取
    @property
    def is_empty(self):
        return not self.path

    def label(self):
        """给标题栏/状态栏用的一行描述。"""
        if self.is_empty:
            return "未选择素材"
        name = os.path.basename(self.path.rstrip("\\/")) or self.path
        if self.kind == "series":
            return f"序列 {name}"
        return f"{KIND_WORDS.get(self.kind, '')} {name}"

    def scan(self, allow_gaps=False):
        """序列目录 -> scan_series 结果；异常原样抛出由调用方显示。"""
        if self.kind != "series":
            return None
        if self.series is None or self.series.get("dir") != self.path:
            self.series = video.scan_series(self.path, allow_gaps=allow_gaps)
            self.series["dir"] = self.path
        return self.series
