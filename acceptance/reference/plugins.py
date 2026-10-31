# -*- coding: utf-8 -*-
"""轻量插件：用户目录里的单个 .py 文件，默认全部关闭。

插件文件约定：
    NAME = "示例"
    VERSION = "1.0"
    DESCRIPTION = "一句话说明"
    def register(api):
        api.add_action("插件", "打个招呼", lambda: api.notify("你好"))

只暴露一个很小的 API：往菜单里加动作、写状态栏、打开素材。插件不能改
工作台内部状态，加载失败也只记一条错误，不影响主程序启动。

扫描阶段只用 ast 读元数据，不执行插件代码；只有用户勾选启用的插件才会
被导入。禁用文件和普通文本一样安全。
"""
import ast
import importlib.util
import os
import re
import tokenize

from PySide6 import QtGui

from reference.gui import prefs


class Plugin:
    __slots__ = ("key", "path", "name", "version", "description", "error")

    def __init__(self, path):
        self.path = path
        self.key = os.path.splitext(os.path.basename(path))[0]
        self.name = self.key
        self.version = ""
        self.description = ""
        self.error = ""


def _enabled_keys():
    value = prefs.get("plugins/enabled", [])
    if isinstance(value, str):
        value = [value]
    if not isinstance(value, (list, tuple)):
        return set()
    return {str(v) for v in value if str(v).strip()}


def is_enabled(plugin):
    return plugin.key in _enabled_keys()


def set_enabled(key, on):
    keys = _enabled_keys()
    if on:
        keys.add(key)
    else:
        keys.discard(key)
    prefs.put("plugins/enabled", sorted(keys))


def _metadata(path):
    """只解析不执行：常量必须写成字面量，函数只找 register 的名字。"""
    with tokenize.open(path) as f:
        tree = ast.parse(f.read(), filename=path)
    meta = {}
    for node in tree.body:
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id in (
                        "NAME", "VERSION", "DESCRIPTION"):
                    try:
                        meta[target.id] = ast.literal_eval(node.value)
                    except (ValueError, SyntaxError):
                        pass
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) \
                and node.name == "register":
            meta["register"] = True
    return meta


def discover(directory=None):
    """扫描插件目录；元数据或语法错误都落在 Plugin.error 里。"""
    d = os.path.normpath(directory or prefs.plugin_dir())
    try:
        os.makedirs(d, exist_ok=True)
        names = sorted(os.listdir(d))
    except OSError:
        return []
    out = []
    for filename in names:
        if not filename.endswith(".py") or filename.startswith("_"):
            continue
        p = Plugin(os.path.join(d, filename))
        try:
            meta = _metadata(p.path)
            p.name = str(meta.get("NAME") or p.key)
            p.version = str(meta.get("VERSION") or "")
            p.description = str(meta.get("DESCRIPTION") or "")
            if not meta.get("register"):
                raise TypeError("缺少 register(api) 函数")
        except Exception as e:          # 单个坏文件不能拖垮整次扫描
            p.error = f"{type(e).__name__}: {e}"
        out.append(p)
    return out


def _import(path, key):
    safe = re.sub(r"\W+", "_", key)
    spec = importlib.util.spec_from_file_location(
        f"lapse_user_plugin_{safe}", path)
    if spec is None or spec.loader is None:
        raise ImportError("无法读取插件文件")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class PluginAPI:
    """交给 register() 的唯一入口；动作由外壳统一回收。"""

    def __init__(self, shell):
        self.shell = shell
        self.actions = []              # [(menu, QAction)]

    def add_action(self, menu, text, callback, shortcut=None):
        m = self.shell.menu(str(menu))
        a = QtGui.QAction(str(text), self.shell)
        if shortcut:
            a.setShortcut(shortcut)
        a.triggered.connect(lambda _=False: callback())
        m.addAction(a)
        self.actions.append((m, a))
        return a

    def remove_actions(self):
        for menu, action in self.actions:
            menu.removeAction(action)
            action.deleteLater()
        self.actions = []

    def notify(self, text):
        self.shell.set_status(str(text))

    def open_path(self, path):
        return self.shell.open_path(path)


def load_enabled(shell):
    """加载已启用的插件，返回 (api 列表, 失败说明列表)。"""
    apis, errors = [], []
    for p in discover():
        if not is_enabled(p):
            continue
        if p.error:
            errors.append(f"{p.name}：{p.error}")
            continue
        api = PluginAPI(shell)
        try:
            mod = _import(p.path, p.key)
            register = getattr(mod, "register", None)
            if not callable(register):
                raise TypeError("缺少 register(api) 函数")
            register(api)
        except Exception as e:
            api.remove_actions()       # 注册到一半失败的也收回
            errors.append(f"{p.name}：{type(e).__name__}: {e}")
            continue
        apis.append(api)
    return apis, errors
