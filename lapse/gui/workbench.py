# -*- coding: utf-8 -*-
"""工作台外壳：一个窗口、一套菜单、一条全局进度。

没有左侧模块栏：顶部一排工具标签（调色台 / 对齐 / 合成 / 裁剪 / 配乐 / 运动），
六个工具永远都可打开，素材类型只决定各页自动接什么，不决定能不能进——只拍了一张
照片也能去对齐页手动选目录。调色台是默认工作区，其余工具都是它的延伸，
切换时不另开窗口。菜单、状态栏进度与取消归外壳所有，整个软件只有一处显示
"正在跑什么"。素材通过 self.material 在工具之间流转，项目本身
（project_kind/path）只在用户真正导入时更新，合成出的视频不会把项目类型改掉。

外壳布局：
    菜单栏
    ┌──────────────────────────────────────┐
    │ 顶栏（工具标签 / 项目 / 打开 / 皮肤）  │
    ├──────────────────────────────────────┤
    │ 当前工具页面                          │
    └──────────────────────────────────────┘
    状态栏（唯一进度条）
"""
import os
import sys

from PySide6 import QtCore, QtGui, QtWidgets

from lapse import imio, plugins
from lapse import ff as ffmod
from lapse.ff import find_ffmpeg
from lapse.gui import icons, job as jobs, prefs, theme
from lapse.gui.material import Material

APP_TITLE = "延时工作台"
IMG_EXT = tuple(imio.IMG_EXTENSIONS) + (".webp", ".gif")
RAW_EXT = tuple(sorted(imio.RAW_EXTENSIONS))
VID_EXT = (".mp4", ".mov", ".mkv", ".avi", ".m4v", ".ts", ".mts", ".m2ts",
           ".webm", ".wmv", ".hevc", ".h264")
MEDIA_EXT = IMG_EXT + RAW_EXT + VID_EXT

# 一个对话框里同时列出图片、RAW 与视频：用户不用先想清楚自己拿的是什么
MEDIA_FILTER = (
    "图片与视频 ("
    "*.jpg *.jpeg *.png *.tif *.tiff *.bmp *.webp *.gif "
    "*.rw2 *.cr2 *.cr3 *.nef *.arw *.raf *.orf *.dng *.pef *.srw *.3fr *.rwl "
    "*.mp4 *.mov *.mkv *.avi *.m4v *.ts *.mts *.m2ts *.webm *.wmv *.hevc *.h264)"
    ";;图片 ("
    "*.jpg *.jpeg *.png *.tif *.tiff *.bmp *.webp *.gif "
    "*.rw2 *.cr2 *.cr3 *.nef *.arw *.raf *.orf *.dng *.pef *.srw *.3fr *.rwl)"
    ";;视频 ("
    "*.mp4 *.mov *.mkv *.avi *.m4v *.ts *.mts *.m2ts *.webm *.wmv *.hevc *.h264)"
    ";;所有文件 (*.*)")


def _ext(path):
    return os.path.splitext(path)[1].lower()


def is_media(path):
    return _ext(path) in MEDIA_EXT


def is_video(path):
    return _ext(path) in VID_EXT


def is_raw(path):
    return _ext(path) in RAW_EXT


class DropOverlay(QtWidgets.QWidget):
    """拖文件进窗口时盖在内容区上的提示层。"""

    def __init__(self, parent):
        super().__init__(parent)
        self.setAttribute(QtCore.Qt.WidgetAttribute.WA_TransparentForMouseEvents)
        self.setAttribute(QtCore.Qt.WidgetAttribute.WA_NoSystemBackground)
        self.hide()

    def paintEvent(self, _e):
        p = QtGui.QPainter(self)
        p.setRenderHint(QtGui.QPainter.RenderHint.Antialiasing)
        # 盖在任意内容上的浮层：底色与文字都取皮肤里的 PILL/ON，浅色皮肤不会变黑屏
        p.fillRect(self.rect(), QtGui.QColor(*theme.PILL))
        r = QtCore.QRectF(self.rect()).adjusted(44, 44, -44, -44)
        pen = QtGui.QPen(QtGui.QColor(theme.ACCENT), 2,
                         QtCore.Qt.PenStyle.DashLine)
        p.setPen(pen)
        p.setBrush(QtGui.QColor(theme.ACCENT_SOFT))
        p.drawRoundedRect(r, theme.R_XL, theme.R_XL)
        p.setPen(QtGui.QColor(theme.ON))
        f = self.font()
        f.setPixelSize(theme.fs(20))
        f.setBold(True)
        p.setFont(f)
        p.drawText(r, QtCore.Qt.AlignmentFlag.AlignCenter,
                   "松开以打开素材\n图片 · 视频 · 序列文件夹")
        p.end()


class _DragFilter(QtCore.QObject):
    """把子控件上的文件拖放统一收给外壳。

    QLineEdit 默认会把拖进来的路径当文本插入，日志框也可能吞掉事件；
    在应用层先截住，拖到窗口任何位置都是"打开素材"。
    """

    TYPES = (QtCore.QEvent.Type.DragEnter, QtCore.QEvent.Type.DragMove,
             QtCore.QEvent.Type.DragLeave, QtCore.QEvent.Type.Drop)

    def __init__(self, shell):
        super().__init__(shell)
        self.shell = shell

    def eventFilter(self, obj, ev):
        if ev.type() not in self.TYPES or not isinstance(obj, QtWidgets.QWidget):
            return False
        if obj is not self.shell and not self.shell.isAncestorOf(obj):
            return False
        md = ev.mimeData()
        if ev.type() in (QtCore.QEvent.Type.DragEnter,
                         QtCore.QEvent.Type.DragMove):
            if self.shell._accept_mime(md):
                self.shell._drag_show()
                ev.acceptProposedAction()
                return True
            return False
        if ev.type() == QtCore.QEvent.Type.DragLeave:
            self.shell._drag_hide()
            return False
        paths = self.shell._mime_paths(md)
        if paths:
            self.shell._drag_hide()
            name = self.shell.open_paths(paths)
            if name and self.shell.module(name):
                self.shell.set_module(name)
            ev.acceptProposedAction()
            return True
        return False


class PrefetchJob(jobs.Job):
    """导入一条视频：探测 + 解一张预览帧，全在这个线程里做。

    取消只挡住"发布素材"那一步：ffmpeg 那一帧还在跑，等它自己结束就是
    最强硬的边界——半路强杀会留下截断的 PNG，得不偿失。
    """

    def __init__(self, ff, path):
        super().__init__([])
        self.ff = ff
        self.path = path

    def run(self):
        b = ffmod.import_bundle(
            self.ff, self.path,
            progress=lambda frac, text: self.progress.emit(frac, text),
            stopping=lambda: self.stopping)
        if self.stopping:
            self.done.emit(False, "cancelled")
        else:
            self.done.emit(b is not None, b)


class Workbench(QtWidgets.QMainWindow):
    def __init__(self, modules=(), restore=True):
        """modules: Module 子类的列表，注册顺序就是顶部工具标签顺序。

        restore=False 给“命令行已经指定素材”的启动用：不要再把上次项目
        先载入一遍，大序列会白等一次解码。
        """
        super().__init__()
        # 皮肤要在任何控件建起来之前定下来：图标按当时的 token 上色
        _app = QtWidgets.QApplication.instance()
        theme.set_skin(_app, self._saved_skin())
        # 字号同样要早于控件：自绘的那几处文字直接读 theme.fs()
        theme.set_font(_app, self._saved_font())
        theme.set_corner_mode(self._saved_corner())
        self.setWindowTitle(APP_TITLE)
        self.resize(1600, 980)
        # 再窄右栏就只剩滑块，预览也没有可读尺寸了
        self.setMinimumSize(1180, 740)
        self.ff = find_ffmpeg()
        self.material = Material(self)
        self.material.changed.connect(self._on_material)

        self.pages = QtWidgets.QStackedWidget()
        self.setAcceptDrops(True)          # 窗口边缘与空白处也能接
        self.modules = []
        self.mod_btns = {}
        self.tool_acts = {}
        self.page_of = {}
        self.menu_actions = {}
        self.project_kind = ""
        self.project_path = ""
        self._job = None
        self._on_done = None
        self._import = None
        self._drain = []
        self._active = None
        self._last_dir = self._settings().value("shell/last_dir", "")

        root = QtWidgets.QWidget()
        root.setObjectName("Shell")
        rv = QtWidgets.QVBoxLayout(root)
        rv.setContentsMargins(0, 0, 0, 0)
        rv.setSpacing(0)
        rv.addWidget(self._build_topbar())
        rv.addWidget(self.pages, 1)
        self.setCentralWidget(root)
        self.drop_overlay = DropOverlay(root)

        self._build_status()
        self._build_file_menu()
        # 工具/视图菜单先占位：模块 build() 会往"视图"里加动作，
        # 菜单顺序不能由模块注册顺序决定
        self.menu("工具")
        self.menu("视图")
        for cls in modules:
            self.add_module(cls(self))
        # 常规数字框走统一描边样式；滑块行里的 ValBox 无框，不能套用
        for sp in self.findChildren(QtWidgets.QAbstractSpinBox):
            if sp.objectName() != "ValBox":
                sp.setProperty("boxed", True)
        self._finish_menus()
        self._plugin_apis = []
        if self.modules:
            self.set_module(self._home().name)
        self._sync_project_ui()
        self._restore_window()
        if restore:
            self._restore_last()
        # 插件最后加载：恢复项目/切工具会写状态栏，加载失败的提示要留得住
        self.reload_plugins()

        self._drag_filter = _DragFilter(self)
        QtWidgets.QApplication.instance().installEventFilter(self._drag_filter)

    # ------------------------------------------------------------------ 骨架
    @staticmethod
    def _logo_pixmap(size):
        base = getattr(sys, "_MEIPASS", os.path.dirname(os.path.dirname(
            os.path.dirname(os.path.abspath(__file__)))))
        pm = QtGui.QPixmap(os.path.join(base, "assets", "app.png"))
        if pm.isNull():
            return icons.module_icon("调色台").pixmap(size, size)
        return pm.scaled(size, size, QtCore.Qt.AspectRatioMode.KeepAspectRatio,
                         QtCore.Qt.TransformationMode.SmoothTransformation)

    def _build_topbar(self):
        bar = QtWidgets.QWidget()
        bar.setObjectName("TopBar")
        # 固定高度：图库会把中央页压到只剩子模式栏，别让多余高度跑进顶栏
        bar.setFixedHeight(50)
        lay = QtWidgets.QHBoxLayout(bar)
        lay.setContentsMargins(14, 6, 14, 6)
        lay.setSpacing(8)

        logo = QtWidgets.QLabel()
        logo.setObjectName("TopLogo")
        logo.setFixedSize(28, 28)
        logo.setAlignment(QtCore.Qt.AlignmentFlag.AlignCenter)
        logo.setPixmap(self._logo_pixmap(24))
        logo.setToolTip(APP_TITLE)
        lay.addWidget(logo)

        # 工具标签占外壳位置，模块注册时往里塞按钮
        self.tool_tabs = QtWidgets.QWidget()
        self.tool_tabs.setObjectName("ToolTabs")
        self.tool_tabs_lay = QtWidgets.QHBoxLayout(self.tool_tabs)
        self.tool_tabs_lay.setContentsMargins(0, 0, 0, 0)
        self.tool_tabs_lay.setSpacing(2)
        lay.addWidget(self.tool_tabs)
        lay.addSpacing(6)

        self.lb_page_hint = QtWidgets.QLabel("")
        self.lb_page_hint.setObjectName("PageHint")
        self.lb_page_hint.setSizePolicy(
            QtWidgets.QSizePolicy.Policy.Ignored,
            QtWidgets.QSizePolicy.Policy.Preferred)
        lay.addWidget(self.lb_page_hint, 1)

        self.lb_drop_hint = QtWidgets.QLabel("拖入图片 / 视频 / 文件夹")
        self.lb_drop_hint.setObjectName("DropHint")
        lay.addWidget(self.lb_drop_hint)

        self.lb_material = QtWidgets.QLabel("未导入项目")
        self.lb_material.setObjectName("MaterialChip")
        self.lb_material.setToolTip("当前项目；素材在工具之间会自动接着用")
        lay.addWidget(self.lb_material)

        self.btn_open = QtWidgets.QToolButton()
        self.btn_open.setObjectName("OpenMedia")
        self.btn_open.setText("打开素材")
        icons.bind(self.btn_open, "打开素材", "on_accent", 18)
        self.btn_open.setPopupMode(
            QtWidgets.QToolButton.ToolButtonPopupMode.MenuButtonPopup)
        self.btn_open.setCursor(QtCore.Qt.CursorShape.PointingHandCursor)
        self.btn_open.setToolTip("打开素材（Ctrl+O）\n图片、RAW、视频都在这一个对话框里")
        lay.addWidget(self.btn_open)

        # 皮肤入口放在顶栏而不是埋进菜单：换肤是一眼就能看见的偏好
        self.btn_skin = QtWidgets.QToolButton()
        self.btn_skin.setObjectName("Icon")
        self.btn_skin.setPopupMode(
            QtWidgets.QToolButton.ToolButtonPopupMode.InstantPopup)
        self.btn_skin.setCursor(QtCore.Qt.CursorShape.PointingHandCursor)
        self.btn_skin.setToolTip("换皮肤")
        icons.bind(self.btn_skin, "palette", "dim", 18)
        lay.addWidget(self.btn_skin)

        self.btn_settings = QtWidgets.QToolButton()
        self.btn_settings.setObjectName("Icon")
        self.btn_settings.setCursor(QtCore.Qt.CursorShape.PointingHandCursor)
        self.btn_settings.setToolTip("设置（Ctrl+,）")
        icons.bind(self.btn_settings, "settings", "dim", 18)
        self.btn_settings.clicked.connect(self.open_settings)
        lay.addWidget(self.btn_settings)
        return bar

    def _build_status(self):
        sb = self.statusBar()
        self.lb_status = QtWidgets.QLabel("")
        self.lb_detail = QtWidgets.QLabel("")
        self.prog = QtWidgets.QProgressBar()
        self.prog.setRange(0, 1000)
        self.prog.setTextVisible(False)
        self.prog.setFixedWidth(180)
        self.prog.setVisible(False)
        self.btn_cancel = QtWidgets.QPushButton("取消")
        self.btn_cancel.setVisible(False)
        self.btn_cancel.clicked.connect(self.cancel_job)
        sb.addWidget(self.lb_status, 1)
        sb.addPermanentWidget(self.prog)
        sb.addPermanentWidget(self.lb_detail)
        sb.addPermanentWidget(self.btn_cancel)
        self._sync_status_vis()

    def _sync_status_vis(self):
        """状态栏没内容就整条隐藏：空着一条黑带比没有更糟。"""
        self.statusBar().setVisible(bool(self.lb_status.text())
                                    or self.prog.isVisible()
                                    or bool(self.lb_detail.text()))

    def _build_file_menu(self):
        # 打开动作归外壳：不管当前在哪个模块，Ctrl+O 看到的都是同一套媒体类型
        m = self.menu("文件")
        self.act_open_media = QtGui.QAction("打开素材…", self)
        self.act_open_media.setShortcut("Ctrl+O")
        self.act_open_media.triggered.connect(self.open_media_dialog)
        self.act_open_folder = QtGui.QAction("打开文件夹…", self)
        self.act_open_folder.setShortcut("Ctrl+Shift+O")
        self.act_open_folder.triggered.connect(self.open_folder_dialog)
        m.addAction(self.act_open_media)
        m.addAction(self.act_open_folder)
        m.addSeparator()

        menu = QtWidgets.QMenu(self.btn_open)
        menu.addAction(self.act_open_media)
        menu.addAction(self.act_open_folder)
        self.btn_open.setMenu(menu)
        self.btn_open.clicked.connect(self.open_media_dialog)

    def _finish_menus(self):
        m = self.menu("文件")
        m.addSeparator()
        self.act_settings = QtGui.QAction("设置…", self, shortcut="Ctrl+,",
                                          triggered=self.open_settings)
        m.addAction(self.act_settings)
        m.addSeparator()
        m.addAction(QtGui.QAction("退出", self, shortcut="Ctrl+Q",
                                  triggered=self.close))
        self._build_skin_menu()

    # ------------------------------------------------------------------ 设置
    def open_settings(self):
        from lapse.gui import settingsdialog
        settingsdialog.SettingsDialog(self).exec()

    def reload_plugins(self):
        for api in getattr(self, "_plugin_apis", ()):
            api.remove_actions()
        self._plugin_apis, errors = plugins.load_enabled(self)
        if errors:
            self.set_status("插件加载失败：" + "；".join(errors[:2]))
        return errors

    def refresh_recent(self):
        dev = self.module("调色台")
        if dev is not None and hasattr(dev, "refresh_start"):
            dev.refresh_start()

    def _restore_window(self):
        if not prefs.as_bool(prefs.get("window/remember", True), True):
            return
        geo = self._settings().value("window/geometry")
        if isinstance(geo, QtCore.QByteArray) and not geo.isEmpty():
            self.restoreGeometry(geo)

    def _restore_last(self):
        """恢复上次的项目；路径没了就静默跳过并清掉记录。"""
        if not prefs.as_bool(prefs.get("general/restore_last", True), True):
            return
        path = str(prefs.get("shell/last_project", "") or "")
        if not path:
            return
        if not os.path.exists(path):
            prefs.put("shell/last_project", "")
            return
        self.open_path(path)
        mod = str(prefs.get("shell/last_module", "") or "")
        if mod and self.module(mod):
            self.set_module(mod)

    # ------------------------------------------------------------------ 皮肤
    def _saved_skin(self):
        key = self._settings().value("shell/skin", theme.DEFAULT_SKIN)
        return key if key in theme.SKINS else theme.DEFAULT_SKIN

    def _saved_corner(self):
        v = self._settings().value("shell/corner", theme.CORNER)
        try:
            v = int(v)
        except (TypeError, ValueError):
            return theme.CORNER
        return v if v in {m for _l, m in theme.CORNER_MODES} else theme.CORNER

    def _saved_font(self):
        key = self._settings().value("shell/font", theme.DEFAULT_FONT)
        names = [k for k, _s in theme.FONT_LEVELS]
        return key if key in names else theme.DEFAULT_FONT

    def set_skin(self, key):
        """换皮肤：写偏好 → 换 token/QSS/调色板 → 图标重画 → 标题栏跟色。"""
        theme.set_skin(QtWidgets.QApplication.instance(), key)
        self._settings().setValue("shell/skin", key)
        for k, a in self.skin_acts.items():
            a.setChecked(k == theme.SKIN)
        self.set_status("皮肤：" + theme.label(theme.SKIN))

    def set_corner(self, mode):
        """窗口弧线：写偏好 → 重新贴给可见窗口（Win10 不认，只是没效果）。"""
        theme.set_corner_mode(mode)
        self._settings().setValue("shell/corner", int(mode))
        self.set_status("窗口弧线：" + theme.corner_label(mode))

    def set_font(self, key):
        """字号档位：写偏好 → 整份 QSS 重建 → 每一页的字跟着一起变。"""
        theme.set_font(QtWidgets.QApplication.instance(), key)
        self._settings().setValue("shell/font", key)
        self.set_status("字号：" + key)

    def _build_skin_menu(self):
        menu = QtWidgets.QMenu(self.btn_skin)
        self.skin_acts = {}
        for key in theme.SKINS:
            a = QtGui.QAction(theme.label(key), self, checkable=True)
            a.triggered.connect(lambda _=False, k=key: self.set_skin(k))
            menu.addAction(a)
            self.skin_acts[key] = a
        self.btn_skin.setMenu(menu)
        # 菜单项归外壳的"视图"，和模块自己塞进去的动作排在一起
        sm = self.menu("视图").addMenu("皮肤")
        for a in self.skin_acts.values():
            sm.addAction(a)
        for k, a in self.skin_acts.items():
            a.setChecked(k == theme.SKIN)

    def menu(self, name):
        """模块取菜单：有则复用，没有则新建（模块注册顺序决定菜单顺序）。"""
        if name not in self.menu_actions:
            self.menu_actions[name] = self.menuBar().addMenu(name)
        return self.menu_actions[name]

    # ------------------------------------------------------------------ 模块
    def add_module(self, mod):
        page = mod.build()
        if page is None:
            raise ValueError(f"模块 {mod.name} 的 build() 没有返回页面")
        self.pages.addWidget(page)
        self.page_of[mod.name] = page
        self.modules.append(mod)
        for d in mod.docks:
            d.setVisible(False)

        n = len(self.mod_btns) + 1
        b = QtWidgets.QToolButton()
        b.setObjectName("ToolTab")
        b.setText(mod.name)
        b.setToolButtonStyle(
            QtCore.Qt.ToolButtonStyle.ToolButtonTextBesideIcon)
        icon = mod.name if icons.has(mod.name) else "circle"
        icons.bind(b, icon, "dim", 17, checked_role="on")
        b.setIconSize(QtCore.QSize(17, 17))
        b.setMinimumHeight(32)
        b.setAccessibleName(mod.name)
        b.setToolTip(f"{mod.name}  (Alt+{n})")
        b.setCheckable(True)
        b.setAutoRaise(True)
        b.setCursor(QtCore.Qt.CursorShape.PointingHandCursor)
        b.clicked.connect(lambda _=False, name=mod.name: self.set_module(name))
        self.tool_tabs_lay.addWidget(b)
        self.mod_btns[mod.name] = b

        # 菜单里也留一份：快捷键归 QAction，菜单状态和标签按钮同步
        a = QtGui.QAction(mod.name, self, checkable=True)
        a.setShortcut(QtGui.QKeySequence(f"Alt+{n}"))
        a.triggered.connect(lambda _=False, name=mod.name: self.set_module(name))
        self.menu("工具").addAction(a)
        self.tool_acts[mod.name] = a

    def module(self, name):
        return next((m for m in self.modules if m.name == name), None)

    def _home(self):
        """默认工作区：调色台；没有它时用注册的第一个模块。"""
        return self.module("调色台") or (self.modules[0] if self.modules else None)

    def set_module(self, name):
        mod = self.module(name)
        if mod is None:
            return False
        if self._active is not None and self._active is not mod:
            self._active.leave()
        self._active = mod
        self.pages.setCurrentWidget(self.page_of[name])
        for k, b in self.mod_btns.items():
            b.setChecked(k == name)
        for k, a in self.tool_acts.items():
            a.setChecked(k == name)
        for m in self.modules:
            if m is not mod:
                for d in m.docks:
                    d.setVisible(False)
        mod.enter()
        self.refresh_docks()
        self.lb_page_hint.setText(mod.top_hint())
        self.set_status(mod.hint())
        return True

    def set_page_hint(self, text):
        """模块内部换页（如调色台子模式）时同步顶栏那句提示。"""
        self.lb_page_hint.setText(text)

    def refresh_docks(self):
        """停靠面板由当前模块决定：调色台切子模式时也调用，外壳不猜。"""
        if self._active is None:
            return
        show = self._active.visible_docks()
        for d in self._active.docks:
            d.setVisible(d in show)

    # ------------------------------------------------------------------ 打开素材
    def _settings(self):
        return QtCore.QSettings(QtCore.QSettings.defaultFormat(),
                                QtCore.QSettings.Scope.UserScope,
                                "lapse", "延时工作台")

    def _remember_dir(self, path):
        d = path if os.path.isdir(path) else os.path.dirname(path)
        if d:
            self._last_dir = os.path.normpath(d)
            self._settings().setValue("shell/last_dir", self._last_dir)

    def open_media_dialog(self):
        files, _ = QtWidgets.QFileDialog.getOpenFileNames(
            self, "打开素材", self._last_dir, MEDIA_FILTER)
        if not files:
            return
        self._remember_dir(files[0])
        name = self.open_paths(files)
        if name and self.module(name):
            self.set_module(name)

    def open_folder_dialog(self):
        d = QtWidgets.QFileDialog.getExistingDirectory(
            self, "打开序列文件夹", self._last_dir)
        if not d:
            return
        self._remember_dir(d)
        name = self.open_path(d)
        if name and self.module(name):
            self.set_module(name)

    def _set_project(self, kind, path):
        """用户真正导入的东西才是"项目"；工具产出的新素材只更新 Material。"""
        self.project_kind = kind
        self.project_path = os.path.normpath(path)
        prefs.add_recent(self.project_path)
        prefs.put("shell/last_project", self.project_path)
        self._sync_project_ui()

    def _sync_project_ui(self):
        if not self.project_path:
            self.lb_material.setText("未导入项目")
            self.lb_material.setToolTip("先导入图片、视频或图片序列文件夹")
            self.setWindowTitle(APP_TITLE)
            self.lb_drop_hint.setVisible(self.material.is_empty)
            return
        name = os.path.basename(self.project_path.rstrip("\\/")) \
            or self.project_path
        word = {"series": "序列", "image": "图片", "video": "视频"}.get(
            self.project_kind, "")
        self.lb_material.setText(f"{word} {name}")
        self.lb_material.setToolTip(
            f"当前项目：{self.project_path}\n当前素材：{self.material.label()}"
            "\n工具会接着用当前素材")
        self.setWindowTitle(f"{name} · {APP_TITLE}")
        self.lb_drop_hint.setVisible(False)

    def open_path(self, path):
        """把一个路径变成当前项目，返回该停下的工具名。

        导入后停在调色台工作区，六个工具随时可切：素材只是自动填进各页，
        用户想先看素材还是直接进工具，由顶部标签自己决定。
        """
        path = os.path.normpath(path)
        if os.path.isdir(path):
            self._set_project("series", path)
            ok = self.material.set_series(path)
        elif is_video(path):
            self._set_project("video", path)
            # 探测和抽帧都在后台：主线程一卡，窗口连拖都拖不动
            ok = self._import_video(path)
        else:
            self._set_project("image", path)
            ok = self.material.set_image(path)
        if not ok:
            # 和当前素材是同一个文件：去重会把它吞掉，但用户就是要点开它
            self.material.reopen()
        home = self._home()
        return home.name if home is not None else None

    def _import_video(self, path):
        """视频导入的唯一入口，返回 True 表示"这件事我接了"。

        后台还没跑完时不发布素材：各页的 on_material 会立刻去解帧，
        缓存没热就等于把卡顿从导入挪到了切页。
        """
        if not self.ff or ffmod.cached_bundle(path) is not None:
            return self._publish_video(path)
        if self._import is not None and self._import.isRunning():
            # 上一条还在解：结果丢掉，但线程对象要留到它自己跑完，
            # 引用在半路被回收等于让 QThread 在运行中销毁
            self._drain.append(self._import)
            self._import.cancel()
        job = PrefetchJob(self.ff, path)
        self._import = job
        job.progress.connect(self._on_progress)
        job.done.connect(self._on_import_done)
        self.prog.setValue(0)
        self.prog.setVisible(True)
        self.btn_cancel.setVisible(True)
        self.set_status(f"正在导入 {os.path.basename(path)}")
        job.start()
        return True

    def _publish_video(self, path):
        if not self.material.set_video(path):
            self.material.reopen()
        return True

    def _on_import_done(self, ok, res):
        job = self.sender()
        if job is not self._import:
            if job in self._drain:
                self._drain.remove(job)
            return
        self._import = None
        if self._job is None or not self._job.isRunning():
            self.prog.setVisible(False)
            self.btn_cancel.setVisible(False)
            self.lb_detail.setText("")
            self._sync_status_vis()
        name = os.path.basename(job.path)
        if ok:
            self._publish_video(job.path)
        elif res == "cancelled":
            self.set_status(f"已取消导入 {name}")
        else:
            self.set_status(f"导入失败：{name} 读不出视频流")

    def open_paths(self, paths):
        """多文件：先问调色台能不能整批接手（它同时认图片和视频）。"""
        paths = [os.path.normpath(p) for p in paths if os.path.exists(p)]
        if not paths:
            return None
        if len(paths) == 1:
            return self.open_path(paths[0])
        files = [p for p in paths if os.path.isfile(p) and is_media(p)]
        if files:
            for m in self.modules:
                if m.open_media_list(files):
                    kind = "video" if all(is_video(p) for p in files) \
                        else "image"
                    self._set_project(kind, files[0])
                    # 批量已经在图库里了：Material 只指向第一张，别再触发单张重载
                    if kind == "video":
                        self.material.set_video(files[0])
                    else:
                        self.material.set_image(files[0])
                    return m.name
        return self.open_path(files[0] if files else paths[0])

    # ---- 拖放：窗口事件与全局过滤器共用这两个判断 ----
    @staticmethod
    def _local_paths(mime):
        return [u.toLocalFile() for u in mime.urls() if u.isLocalFile()]

    def _accept_mime(self, mime):
        paths = self._local_paths(mime)
        return bool(paths) and all(os.path.isdir(p) or is_media(p) for p in paths)

    def _mime_paths(self, mime):
        return self._local_paths(mime) if self._accept_mime(mime) else []

    def _drag_show(self):
        self.drop_overlay.setGeometry(self.centralWidget().rect())
        self.drop_overlay.raise_()
        self.drop_overlay.show()

    def _drag_hide(self):
        self.drop_overlay.hide()

    def dragEnterEvent(self, ev):
        if self._accept_mime(ev.mimeData()):
            self._drag_show()
            ev.acceptProposedAction()
        else:
            ev.ignore()

    def dragLeaveEvent(self, ev):
        self._drag_hide()
        super().dragLeaveEvent(ev)

    def dropEvent(self, ev):
        paths = self._mime_paths(ev.mimeData())
        self._drag_hide()
        if not paths:
            ev.ignore()
            return
        name = self.open_paths(paths)
        if name and self.module(name):
            self.set_module(name)
        ev.acceptProposedAction()

    def resizeEvent(self, ev):
        super().resizeEvent(ev)
        if getattr(self, "drop_overlay", None) is not None:
            self.drop_overlay.setGeometry(self.centralWidget().rect())

    def _on_material(self, kind, path):
        self._sync_project_ui()
        for m in self.modules:
            m.on_material(kind, path)

    # ------------------------------------------------------------------ 任务
    def start_job(self, job, on_done, card=None):
        """后台任务的唯一入口：进度条与取消按钮归状态栏。

        card 给了就把这条任务的时间线接到那张结果卡上：模块不必各自
        记得 connect，漏一次就是一条没有进度的黑任务。

        返回 False 表示没启动（已有任务在跑），调用方据此还原按钮状态。
        """
        if self._job is not None and self._job.isRunning():
            QtWidgets.QMessageBox.information(self, APP_TITLE,
                                              "已有任务在跑，先等它结束或取消。")
            return False
        self._on_done = on_done
        self._job = job
        job.progress.connect(self._on_progress)
        job.done.connect(self._on_job_done)
        if card is not None:
            card.set_steps(job.timeline)
            job.stage.connect(card.stage)
        self.prog.setValue(0)
        self.prog.setVisible(True)
        self.btn_cancel.setVisible(True)
        job.start()
        return True

    def _on_progress(self, frac, detail):
        if self.sender() is self._import and self._job is not None \
                and self._job.isRunning():
            return          # 前台任务正在说话，导入那点进度不许插嘴
        self.prog.setValue(int(frac * 1000))
        self.lb_detail.setText(f"{frac * 100:4.1f}%  {detail}")
        self._sync_status_vis()

    def cancel_job(self):
        if self._import is not None and self._import.isRunning():
            self._import.cancel()
        if self._job is not None and self._job.isRunning():
            self._job.cancel()

    def _on_job_done(self, ok, res):
        self.prog.setVisible(False)
        self.btn_cancel.setVisible(False)
        self.lb_detail.setText("")
        self._sync_status_vis()
        cb, self._on_done = self._on_done, None
        if cb:
            cb(ok, res)

    def set_status(self, text):
        self.lb_status.setText(text)
        self._sync_status_vis()

    def is_active(self, mod):
        """模块问"现在轮到我说话吗"，用来避免后台模块抢状态栏。"""
        return self._active is mod

    # ------------------------------------------------------------------ 收尾
    def closeEvent(self, e):
        for job in [self._import] + self._drain:
            if job is not None and job.isRunning():
                # 导入只是解一帧预览，等它自然收尾就行，不值得弹窗拦用户
                job.cancel()
                job.wait(3000)
        if self._job is not None and self._job.isRunning():
            if QtWidgets.QMessageBox.question(
                    self, APP_TITLE, "正在处理，确定要退出吗？") \
                    != QtWidgets.QMessageBox.StandardButton.Yes:
                e.ignore()
                return
            self._job.cancel()
            self._job.wait(4000)
        if prefs.as_bool(prefs.get("window/remember", True), True):
            self._settings().setValue("window/geometry", self.saveGeometry())
        if self._active is not None:
            prefs.put("shell/last_module", self._active.name)
        app = QtWidgets.QApplication.instance()
        if app is not None and getattr(self, "_drag_filter", None) is not None:
            app.removeEventFilter(self._drag_filter)
        for m in self.modules:
            m.shutdown()
        super().closeEvent(e)
