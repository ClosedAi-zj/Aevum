# -*- coding: utf-8 -*-
"""模块基类：外壳与工具面板之间的唯一契约。

一个模块 = 一块中央页 + 若干可选停靠面板。外壳负责菜单、顶部工具切换、
状态栏进度与取消；模块只在自己的 build()/enter()/leave()/on_material() 里活动，
互相之间不引用，靠 Material 传素材。
"""
from PySide6 import QtCore, QtWidgets


class Module(QtCore.QObject):
    name = ""
    # 模块拥有的停靠面板；外壳按 visible_docks() 决定进入时显示哪几个
    docks = ()

    def __init__(self, shell):
        super().__init__(shell)
        self.shell = shell

    def build(self):
        """返回本页控件；此时可以往 shell 上加 dock 和菜单项。"""
        raise NotImplementedError

    def enter(self):
        pass

    def leave(self):
        pass

    def hint(self):
        """状态栏左侧的提示语：只放状态（张数/进度），教程文案进悬停提示。"""
        return ""

    def top_hint(self):
        """顶栏标题旁的一句短提示；默认与状态栏相同。"""
        return self.hint()

    def sync_page_hint(self):
        """本模块状态变了就把它同步到顶栏（顶栏只有一条，谁在前台谁说话）。"""
        if self.shell.is_active(self):
            self.shell.set_page_hint(self.top_hint())

    def settings(self):
        """模块私有的记忆（上次目录/参数）。键名以模块名开头，互不串台。

        显式传格式：PySide6 里 setDefaultFormat(IniFormat) 管不到两参数构造，
        测试要靠 setPath 把偏好落到临时 INI，不能碰用户真实注册表。
        """
        return QtCore.QSettings(QtCore.QSettings.defaultFormat(),
                                QtCore.QSettings.Scope.UserScope,
                                "lapse", "延时工作台")

    def visible_docks(self):
        """本模块当前该显示哪些停靠面板（模块内部子模式可再细分）。"""
        return self.docks

    def on_material(self, kind, path):
        """流水线：素材变了要不要跟着装载。默认不动，避免互相触发。"""
        pass

    def open_media_list(self, paths):
        """一次拖入/选中多个媒体文件时，愿意接手的模块装载并返回 True。"""
        return False

    def shutdown(self):
        pass

    # 便捷转发：模块里所有后台活都走外壳，进度条与取消按钮全局唯一
    def start_job(self, job, on_done, card=None):
        # 必须把外壳的返回值传出去：False 表示没启动，模块要据此还原按钮
        return self.shell.start_job(job, on_done, card)

    def status(self, text):
        # 状态栏全应用只有一条，谁在前台谁说话：流水线会让后台模块也装载素材，
        # 它们那句"预览工作图 …"盖掉用户正在看的提示。
        if self.shell.is_active(self):
            self.shell.set_status(text)

    def warn(self, text, title=None):
        QtWidgets.QMessageBox.warning(self.shell, title or self.name, text)
