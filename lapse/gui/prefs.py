# -*- coding: utf-8 -*-
"""用户偏好与缓存目录：设置对话框、起始页、缩略图缓存共用的唯一入口。

QSettings 的格式/作用域必须显式传，单靠 setDefaultFormat 管不到两参数构造；
测试用 setPath 把偏好落到临时 INI，不会碰用户真实配置。
"""
import os
import shutil
import sys

from PySide6 import QtCore

ORG, APP = "lapse", "延时工作台"


def settings():
    return QtCore.QSettings(QtCore.QSettings.defaultFormat(),
                            QtCore.QSettings.Scope.UserScope, ORG, APP)


def as_bool(value, default=False):
    if value is None:
        return default
    if isinstance(value, bool):
        return value
    return str(value).strip().lower() in ("1", "true", "yes", "on")


def get(key, default=None):
    return settings().value(key, default)


def put(key, value):
    s = settings()
    s.setValue(key, value)
    s.sync()


def _roaming_root():
    if sys.platform == "win32":
        base = os.environ.get("APPDATA") or os.path.expanduser(
            os.path.join("~", "AppData", "Roaming"))
    else:
        base = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser(
            os.path.join("~", ".config"))
    return os.path.join(base, "lapse")


def _local_root():
    if sys.platform == "win32":
        base = os.environ.get("LOCALAPPDATA") or os.path.expanduser(
            os.path.join("~", "AppData", "Local"))
    else:
        base = os.environ.get("XDG_CACHE_HOME") or os.path.expanduser(
            os.path.join("~", ".cache"))
    return os.path.join(base, "lapse")


def default_cache_dir():
    return os.path.join(_local_root(), "cache")


def cache_dir():
    raw = str(get("cache/dir", "") or "").strip()
    return os.path.normpath(os.path.expanduser(raw)) if raw \
        else default_cache_dir()


def thumb_cache_enabled():
    return as_bool(get("cache/thumbs", True), True)


def thumb_cache_dir():
    return os.path.join(cache_dir(), "thumbs")


def plugin_dir():
    raw = str(get("plugins/dir", "") or "").strip()
    return os.path.normpath(os.path.expanduser(raw)) if raw \
        else os.path.join(_roaming_root(), "plugins")


def cache_size(path=None):
    """目录里所有文件的总字节数；目录不存在返回 0。"""
    root = os.path.normpath(path or cache_dir())
    if not os.path.isdir(root):
        return 0
    total = 0
    for base, _dirs, files in os.walk(root):
        for name in files:
            try:
                total += os.path.getsize(os.path.join(base, name))
            except OSError:
                pass
    return total


def human_size(n):
    n = float(max(0, n))
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024 or unit == "GB":
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024.0
    return f"{n:.1f} GB"


def clear_cache():
    """只清应用自己的缓存目录，不动用户素材。返回删除的字节数。"""
    root = os.path.normpath(cache_dir())
    freed = cache_size(root)
    if root and os.path.isdir(root):
        shutil.rmtree(root, ignore_errors=True)
    os.makedirs(root, exist_ok=True)
    return freed


def recent_projects():
    value = get("shell/recent", [])
    if isinstance(value, str):
        value = [value]
    if not isinstance(value, (list, tuple)):
        return []
    return [os.path.normpath(str(p)) for p in value if str(p).strip()]


def add_recent(path, limit=None):
    path = os.path.normpath(str(path))
    if not path or not os.path.exists(path):
        return
    n = int(limit if limit is not None else get("general/recent_count", 8))
    n = max(1, min(20, n))
    items = [p for p in recent_projects()
             if os.path.normcase(p) != os.path.normcase(path)]
    items.insert(0, path)
    put("shell/recent", items[:n])


def remove_recent(path):
    key = os.path.normcase(os.path.normpath(path))
    put("shell/recent", [p for p in recent_projects()
                         if os.path.normcase(p) != key])
