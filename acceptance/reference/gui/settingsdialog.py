# -*- coding: utf-8 -*-
"""设置：常规 / 缓存 / 界面 / 插件 / 关于。

改动即时生效并写进 QSettings，没有"取消"——只有一个"完成"。
缓存只清应用自己的缩略图目录，永远不碰用户素材。
"""
import os
import sys

from PySide6 import QtCore, QtGui, QtWidgets

from reference import __version__, ff
from reference import plugins as pluginmod
from reference.gui import hints, prefs, theme

PAGES = ("常规", "缓存", "界面", "插件", "关于")


def _note(text):
    lb = QtWidgets.QLabel(text)
    lb.setObjectName("SettingsHint")
    lb.setWordWrap(True)
    return lb


def _path_label(path):
    lb = QtWidgets.QLabel(path)
    lb.setObjectName("SettingsPath")
    lb.setWordWrap(True)
    lb.setTextInteractionFlags(QtCore.Qt.TextInteractionFlag.TextSelectableByMouse)
    return lb


class SettingsDialog(QtWidgets.QDialog):
    def __init__(self, shell):
        super().__init__(shell)
        self.shell = shell
        self.setWindowTitle("设置")
        self.setWindowFlag(
            QtCore.Qt.WindowType.WindowContextHelpButtonHint, False)
        self.setModal(True)
        self.resize(860, 590)
        self.setMinimumSize(760, 520)

        root = QtWidgets.QHBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)

        self.nav = QtWidgets.QListWidget()
        self.nav.setObjectName("SettingsNav")
        self.nav.setFixedWidth(168)
        self.nav.setFrameShape(QtWidgets.QFrame.Shape.NoFrame)
        self.nav.setFocusPolicy(QtCore.Qt.FocusPolicy.NoFocus)
        for name in PAGES:
            item = QtWidgets.QListWidgetItem(name)
            item.setSizeHint(QtCore.QSize(0, 40))
            self.nav.addItem(item)
        root.addWidget(self.nav)

        right = QtWidgets.QVBoxLayout()
        right.setContentsMargins(0, 0, 0, 0)
        right.setSpacing(0)
        self.stack = QtWidgets.QStackedWidget()
        right.addWidget(self.stack, 1)
        foot = QtWidgets.QHBoxLayout()
        foot.setContentsMargins(24, 10, 24, 14)
        foot.addStretch(1)
        b = QtWidgets.QPushButton("完成")
        b.setObjectName("Primary")
        b.clicked.connect(self.accept)
        foot.addWidget(b)
        right.addLayout(foot)
        root.addLayout(right, 1)

        self.stack.addWidget(self._page_general())
        self.stack.addWidget(self._page_cache())
        self.stack.addWidget(self._page_ui())
        self.stack.addWidget(self._page_plugins())
        self.stack.addWidget(self._page_about())
        self.nav.currentRowChanged.connect(self.stack.setCurrentIndex)
        self.nav.setCurrentRow(0)

    # ------------------------------------------------------------------ 通用
    def _page(self, title, hint):
        w = QtWidgets.QWidget()
        v = QtWidgets.QVBoxLayout(w)
        v.setContentsMargins(26, 22, 26, 16)
        v.setSpacing(12)
        t = QtWidgets.QLabel(title)
        t.setObjectName("SettingsTitle")
        v.addWidget(t)
        v.addWidget(_note(hint))
        body = QtWidgets.QVBoxLayout()
        body.setSpacing(12)
        v.addLayout(body)
        v.addStretch(1)
        return w, body

    def _page_general(self):
        page, body = self._page("常规", "启动时做什么，以及最近打开保留多少")
        self.cb_restore = QtWidgets.QCheckBox("启动时恢复上次打开的项目")
        self.cb_restore.setChecked(prefs.as_bool(
            prefs.get("general/restore_last", True), True))
        hints.attach(self.cb_restore, "设置/恢复项目")
        self.cb_restore.toggled.connect(
            lambda on: prefs.put("general/restore_last", bool(on)))
        body.addWidget(self.cb_restore)

        self.cb_edits = QtWidgets.QCheckBox("自动保存调色参数")
        self.cb_edits.setChecked(prefs.as_bool(
            prefs.get("edits/autosave", True), True))
        hints.attach(self.cb_edits, "设置/自动保存")
        self.cb_edits.toggled.connect(
            lambda on: prefs.put("edits/autosave", bool(on)))
        body.addWidget(self.cb_edits)

        row = QtWidgets.QHBoxLayout()
        row.setSpacing(10)
        lb_recent = QtWidgets.QLabel("最近打开保留条数")
        row.addWidget(lb_recent)
        self.sp_recent = QtWidgets.QSpinBox()
        self.sp_recent.setRange(1, 20)
        self.sp_recent.setFixedWidth(78)
        self.sp_recent.setValue(max(1, min(20, int(
            prefs.get("general/recent_count", 8) or 8))))
        hints.attach(self.sp_recent, "设置/最近条数", label=lb_recent)
        self.sp_recent.valueChanged.connect(self._set_recent_count)
        row.addWidget(self.sp_recent)
        row.addStretch(1)
        b_clear = QtWidgets.QPushButton("清空最近打开")
        b_clear.setObjectName("Danger")
        b_clear.clicked.connect(self._clear_recent)
        row.addWidget(b_clear)
        body.addLayout(row)
        body.addWidget(_note("最近打开只记路径，不会移动或删除任何素材。"))
        return page

    def _set_recent_count(self, n):
        prefs.put("general/recent_count", int(n))
        items = prefs.recent_projects()
        if len(items) > n:
            prefs.put("shell/recent", items[:n])
        self.shell.refresh_recent()

    def _clear_recent(self):
        prefs.put("shell/recent", [])
        self.shell.refresh_recent()
        self.shell.set_status("最近打开已清空")

    def _page_cache(self):
        page, body = self._page(
            "缓存", "缩略图缓存让再次打开大目录时不用重新解码")

        row = QtWidgets.QHBoxLayout()
        row.setSpacing(6)
        self.ed_cache = QtWidgets.QLineEdit(str(prefs.get("cache/dir", "") or ""))
        self.ed_cache.setPlaceholderText(prefs.default_cache_dir())
        hints.attach(self.ed_cache, "设置/缓存目录")
        self.ed_cache.editingFinished.connect(self._apply_cache_dir)
        row.addWidget(self.ed_cache, 1)
        b = QtWidgets.QPushButton("浏览…")
        b.clicked.connect(self._browse_cache)
        row.addWidget(b)
        b = QtWidgets.QPushButton("恢复默认")
        b.clicked.connect(self._default_cache)
        row.addWidget(b)
        body.addLayout(row)
        self.lb_cache_eff = _path_label("实际目录：" + prefs.cache_dir())
        body.addWidget(self.lb_cache_eff)

        self.cb_thumbs = QtWidgets.QCheckBox("把缩略图缓存到磁盘")
        self.cb_thumbs.setChecked(prefs.thumb_cache_enabled())
        hints.attach(self.cb_thumbs, "设置/缩略图")
        self.cb_thumbs.toggled.connect(
            lambda on: prefs.put("cache/thumbs", bool(on)))
        body.addWidget(self.cb_thumbs)

        row = QtWidgets.QHBoxLayout()
        row.setSpacing(6)
        self.lb_cache_size = QtWidgets.QLabel()
        row.addWidget(self.lb_cache_size)
        row.addStretch(1)
        b = QtWidgets.QPushButton("打开缓存目录")
        b.clicked.connect(self._open_cache_dir)
        row.addWidget(b)
        b = QtWidgets.QPushButton("清理缓存")
        b.setObjectName("Danger")
        b.clicked.connect(self._clear_cache)
        row.addWidget(b)
        body.addLayout(row)
        self._refresh_cache_size()
        body.addWidget(_note(
            "清理只删除缩略图与临时预览，不会碰原始照片、视频或导出的成片。"))
        return page

    def _refresh_cache_size(self):
        self.lb_cache_size.setText(
            "当前占用 " + prefs.human_size(prefs.cache_size()))

    def _apply_cache_dir(self):
        raw = self.ed_cache.text().strip()
        path = os.path.normpath(os.path.expanduser(raw)) if raw else ""
        if path and path == os.path.normpath(prefs.default_cache_dir()):
            path = ""
        prefs.put("cache/dir", path)
        self.ed_cache.setText(path)
        self.lb_cache_eff.setText("实际目录：" + prefs.cache_dir())
        self._refresh_cache_size()

    def _browse_cache(self):
        d = QtWidgets.QFileDialog.getExistingDirectory(
            self, "选择缓存目录", prefs.cache_dir())
        if d:
            self.ed_cache.setText(os.path.normpath(d))
            self._apply_cache_dir()

    def _default_cache(self):
        self.ed_cache.setText("")
        self._apply_cache_dir()

    def _open_cache_dir(self):
        d = prefs.cache_dir()
        os.makedirs(d, exist_ok=True)
        QtWidgets.QDesktopServices.openUrl(QtCore.QUrl.fromLocalFile(d))

    def _clear_cache(self):
        if QtWidgets.QMessageBox.question(
                self, "清理缓存",
                f"删除 {prefs.cache_dir()} 里的缓存文件？\n"
                "原始素材和导出成片不受影响。") \
                != QtWidgets.QMessageBox.StandardButton.Yes:
            return
        freed = prefs.clear_cache()
        self._refresh_cache_size()
        self.shell.set_status("已清理缓存 " + prefs.human_size(freed))

    def _page_ui(self):
        page, body = self._page("界面", "皮肤、字号与窗口弧线；改动立即生效，不用重启")
        row = QtWidgets.QHBoxLayout()
        row.setSpacing(10)
        lb_skin = QtWidgets.QLabel("皮肤")
        row.addWidget(lb_skin)
        self.cb_skin = QtWidgets.QComboBox()
        for key in theme.SKINS:
            self.cb_skin.addItem(theme.label(key), key)
        idx = self.cb_skin.findData(theme.SKIN)
        self.cb_skin.setCurrentIndex(max(0, idx))
        self.cb_skin.setMinimumWidth(250)
        hints.attach(self.cb_skin, "设置/皮肤", label=lb_skin)
        self.cb_skin.currentIndexChanged.connect(self._set_skin)
        row.addWidget(self.cb_skin)
        row.addStretch(1)
        body.addLayout(row)

        row = QtWidgets.QHBoxLayout()
        row.setSpacing(10)
        lb_font = QtWidgets.QLabel("字号")
        row.addWidget(lb_font)
        self.cb_font = QtWidgets.QComboBox()
        for name, _scale in theme.FONT_LEVELS:
            self.cb_font.addItem(name, name)
        self.cb_font.setCurrentIndex(max(
            0, self.cb_font.findData(theme.FONT)))
        self.cb_font.setMinimumWidth(250)
        hints.attach(self.cb_font, "设置/字号", label=lb_font)
        self.cb_font.currentIndexChanged.connect(self._set_font)
        row.addWidget(self.cb_font)
        row.addStretch(1)
        body.addLayout(row)

        row = QtWidgets.QHBoxLayout()
        row.setSpacing(10)
        lb_corner = QtWidgets.QLabel("窗口圆角")
        row.addWidget(lb_corner)
        self.cb_corner = QtWidgets.QComboBox()
        for name, mode in theme.CORNER_MODES:
            self.cb_corner.addItem(name, mode)
        self.cb_corner.setCurrentIndex(max(
            0, self.cb_corner.findData(theme.CORNER)))
        self.cb_corner.setMinimumWidth(250)
        hints.attach(self.cb_corner, "设置/圆角", label=lb_corner)
        self.cb_corner.currentIndexChanged.connect(self._set_corner)
        row.addWidget(self.cb_corner)
        row.addStretch(1)
        body.addLayout(row)
        self.lb_corner = _note("")
        body.addWidget(self.lb_corner)
        self._refresh_corner_note()

        self.cb_geometry = QtWidgets.QCheckBox("记住窗口大小和位置")
        self.cb_geometry.setChecked(prefs.as_bool(
            prefs.get("window/remember", True), True))
        hints.attach(self.cb_geometry, "设置/窗口记忆")
        self.cb_geometry.toggled.connect(
            lambda on: prefs.put("window/remember", bool(on)))
        body.addWidget(self.cb_geometry)
        body.addWidget(_note(
            "图标、示波器与标题栏会跟着皮肤一起换色，不用重启。\n"
            "每套皮肤的文字与底色都按 WCAG 比值检查过，切换前不用担心看不清。"))
        return page

    def _refresh_corner_note(self):
        name = theme.corner_label(theme.CORNER)
        if sys.platform != "win32":
            self.lb_corner.setText(
                f"当前：{name}。窗口弧线由系统绘制，非 Windows 上这项不起作用。")
        elif theme.CORNER < 0:
            self.lb_corner.setText(f"当前：{name}，形状交给系统决定。")
        elif theme.CORNER_HR == 0:
            self.lb_corner.setText(f"当前：{name}。这台机器支持窗口弧线，已立即应用。")
        elif theme.CORNER_HR is None:
            self.lb_corner.setText(f"当前：{name}，窗口出现时生效。")
        else:
            self.lb_corner.setText(
                f"当前：{name}。这台系统不认窗口弧线属性（一般是 Windows 10"
                " 及更早），选了也不会有变化。")

    def _set_skin(self, i):
        key = self.cb_skin.itemData(i)
        if key:
            self.shell.set_skin(key)

    def _set_corner(self, i):
        mode = self.cb_corner.itemData(i)
        if mode is not None:
            self.shell.set_corner(int(mode))
            self._refresh_corner_note()

    def _set_font(self, i):
        key = self.cb_font.itemData(i)
        if key:
            self.shell.set_font(key)

    def _page_plugins(self):
        page, body = self._page(
            "插件", "插件是插件目录里的单个 .py 文件，默认关闭，启用后立即加载")

        row = QtWidgets.QHBoxLayout()
        row.setSpacing(6)
        self.ed_plugins = QtWidgets.QLineEdit(str(prefs.get("plugins/dir", "") or ""))
        self.ed_plugins.setPlaceholderText(prefs.plugin_dir())
        self.ed_plugins.setToolTip("留空使用默认位置")
        self.ed_plugins.editingFinished.connect(self._apply_plugin_dir)
        row.addWidget(self.ed_plugins, 1)
        b = QtWidgets.QPushButton("浏览…")
        b.clicked.connect(self._browse_plugins)
        row.addWidget(b)
        body.addLayout(row)
        self.lb_plugin_eff = _path_label("实际目录：" + prefs.plugin_dir())
        body.addWidget(self.lb_plugin_eff)

        self.pl_list = QtWidgets.QListWidget()
        self.pl_list.setMinimumHeight(220)
        self.pl_list.setSelectionMode(
            QtWidgets.QAbstractItemView.SelectionMode.NoSelection)
        self.pl_list.itemChanged.connect(self._plugin_toggled)
        body.addWidget(self.pl_list, 1)

        row = QtWidgets.QHBoxLayout()
        row.setSpacing(6)
        self.lb_plugin_state = QtWidgets.QLabel("")
        self.lb_plugin_state.setObjectName("SettingsHint")
        self.lb_plugin_state.setWordWrap(True)
        row.addWidget(self.lb_plugin_state, 1)
        b = QtWidgets.QPushButton("重新扫描")
        b.clicked.connect(self._rescan_plugins)
        row.addWidget(b)
        b = QtWidgets.QPushButton("打开插件文件夹")
        b.clicked.connect(self._open_plugin_dir)
        row.addWidget(b)
        body.addLayout(row)
        body.addWidget(_note(
            "插件文件可以定义 NAME / VERSION / DESCRIPTION 和 register(api)。"
            "加载失败只影响该插件，不会拖垮主程序。"))

        self._scanning = False
        self._scan_plugins()
        return page

    def _apply_plugin_dir(self):
        raw = self.ed_plugins.text().strip()
        path = os.path.normpath(os.path.expanduser(raw)) if raw else ""
        prefs.put("plugins/dir", path)
        self.ed_plugins.setText(path)
        self.lb_plugin_eff.setText("实际目录：" + prefs.plugin_dir())
        self._scan_plugins()
        self.shell.reload_plugins()

    def _browse_plugins(self):
        d = QtWidgets.QFileDialog.getExistingDirectory(
            self, "选择插件目录", prefs.plugin_dir())
        if d:
            self.ed_plugins.setText(os.path.normpath(d))
            self._apply_plugin_dir()

    def _open_plugin_dir(self):
        d = prefs.plugin_dir()
        os.makedirs(d, exist_ok=True)
        QtWidgets.QDesktopServices.openUrl(QtCore.QUrl.fromLocalFile(d))

    def _scan_plugins(self):
        self._scanning = True
        self._plugins = pluginmod.discover()
        self.pl_list.clear()
        for p in self._plugins:
            title = p.name + (f"  v{p.version}" if p.version else "")
            if p.error:
                title += "（加载失败）"
            item = QtWidgets.QListWidgetItem(title)
            item.setData(QtCore.Qt.ItemDataRole.UserRole, p.key)
            item.setToolTip("\n".join(x for x in (
                p.description, p.path, p.error) if x))
            flags = item.flags() | QtCore.Qt.ItemFlag.ItemIsUserCheckable
            if p.error:
                flags &= ~QtCore.Qt.ItemFlag.ItemIsUserCheckable
                item.setForeground(QtGui.QColor(theme.BAD))
            else:
                item.setCheckState(
                    QtCore.Qt.CheckState.Checked if pluginmod.is_enabled(p)
                    else QtCore.Qt.CheckState.Unchecked)
            item.setFlags(flags)
            self.pl_list.addItem(item)
        self._scanning = False
        if not self._plugins:
            self.lb_plugin_state.setText("还没有插件；放一个 .py 文件进来再扫描")
        else:
            bad = [p.name for p in self._plugins if p.error]
            self.lb_plugin_state.setText(
                f"共 {len(self._plugins)} 个插件"
                + (f"，{len(bad)} 个加载失败" if bad else ""))

    def _rescan_plugins(self):
        self._scan_plugins()
        errors = self.shell.reload_plugins()
        if errors:
            self.lb_plugin_state.setText("；".join(errors))

    def _plugin_toggled(self, item):
        if self._scanning:
            return
        key = item.data(QtCore.Qt.ItemDataRole.UserRole)
        on = item.checkState() == QtCore.Qt.CheckState.Checked
        pluginmod.set_enabled(key, on)
        errors = self.shell.reload_plugins()
        self.lb_plugin_state.setText(
            "；".join(errors) if errors else "改动已生效")

    def _page_about(self):
        page, body = self._page(
            "关于", "延时工作台把调色、对齐、合成、裁剪和配乐装进一个窗口")
        name = QtWidgets.QLabel("延时工作台 · Lapse Studio")
        name.setObjectName("AboutName")
        body.addWidget(name)
        body.addWidget(QtWidgets.QLabel(f"版本 {__version__}"))

        form = QtWidgets.QFormLayout()
        form.setContentsMargins(0, 8, 0, 0)
        form.setHorizontalSpacing(18)
        form.setVerticalSpacing(9)
        rows = (
            ("Python", sys.version.split()[0]),
            ("PySide6", _qt_version()),
            ("ffmpeg", ff.find_ffmpeg() or "未找到"),
            ("配置位置", prefs.settings().fileName()),
            ("缓存目录", prefs.cache_dir()),
        )
        for k, v in rows:
            lb = QtWidgets.QLabel(v)
            lb.setWordWrap(True)
            lb.setTextInteractionFlags(
                QtCore.Qt.TextInteractionFlag.TextSelectableByMouse)
            form.addRow(k, lb)
        body.addLayout(form)
        body.addWidget(_note(
            "只借鉴 Lightroom / Photoshop / Capture One 的布局与交互思路，"
            "不含它们的代码或素材。"))
        return page


def _qt_version():
    from PySide6 import __version__ as qt
    return qt
