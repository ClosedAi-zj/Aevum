# -*- coding: utf-8 -*-
"""每帧的星级与色标：存在序列文件夹里，关掉重开还在。

只存评级，不存调色参数（那是 params.py / edits.py 的事）。文件放在序列
文件夹内，素材拷走时评级跟着走，导出的时候也不会被当成图片读进来。
按文件名存：序列内文件名唯一，且整个文件夹改名或换目录都不丢评级。
"""
import json
import os

from reference.gui import edits

FILENAME = ".lapse_ratings.json"
VERSION = 1
MAX_STARS = 5
COLORS = ("none", "red", "yellow", "green", "blue", "purple")
COLOR_NAMES = {"none": "无", "red": "红", "yellow": "黄", "green": "绿",
               "blue": "蓝", "purple": "紫"}
# 色标用固定色，不跟皮肤走：用户记的是「这张标了红」，换皮肤不能变味
COLOR_RGB = {"red": (224, 90, 90), "yellow": (230, 195, 74),
             "green": (87, 194, 106), "blue": (78, 163, 224),
             "purple": (168, 117, 230)}


def color_name(label):
    return COLOR_NAMES.get(label, label)


def color_rgb(label):
    return COLOR_RGB.get(label)


def _key(path):
    return os.path.basename(str(path))


def _clean(entry):
    """把一条记录收敛成 (星级, 色标)；认不出来的一律当没评过。"""
    if not isinstance(entry, dict):
        return 0, "none"
    try:
        stars = int(entry.get("stars", 0))
    except (TypeError, ValueError):
        stars = 0
    label = str(entry.get("label", "none") or "none")
    if label not in COLORS:
        label = "none"
    return max(0, min(MAX_STARS, stars)), label


class Ratings:
    """一个序列文件夹的评级表。folder 为空时只在内存里记，不落盘。"""

    def __init__(self, folder=""):
        raw = os.path.normpath(os.path.expanduser(str(folder or "")))
        self.folder = "" if raw in (".", "") else raw
        self.path = os.path.join(self.folder, FILENAME) if self.folder else None
        self.data = {}
        self.load()

    def load(self):
        self.data = {}
        if not self.path:
            return False
        try:
            with open(self.path, encoding="utf-8") as f:
                raw = json.load(f)
        except (OSError, ValueError):
            return False        # 坏文件只当没有评级，不能拖垮启动
        if not isinstance(raw, dict):
            return False
        entries = raw.get("ratings")
        if not isinstance(entries, dict):
            return False
        for name, entry in entries.items():
            stars, label = _clean(entry)
            if stars or label != "none":
                self.data[str(name)] = (stars, label)
        return True

    def get(self, path):
        """返回 (星级 0-5, 色标名)。"""
        return self.data.get(_key(path), (0, "none"))

    def stars(self, path):
        return self.get(path)[0]

    def label(self, path):
        return self.get(path)[1]

    def set(self, path, stars, label=None):
        """改一帧并立刻落盘；label 传 None 表示只动星级。"""
        self.apply(path, stars, label)
        self.save()

    def set_many(self, paths, stars, label=None):
        for p in paths:
            self.apply(p, stars, label)
        self.save()

    def apply(self, path, stars, label=None):
        """只改内存。全 0（清零 + 无色标）时把条目删掉，文件不会越长越大。"""
        old_stars, old_label = self.get(path)
        new_label = old_label if label is None else (
            label if label in COLORS else "none")
        try:
            new_stars = max(0, min(MAX_STARS, int(stars)))
        except (TypeError, ValueError):
            new_stars = old_stars
        key = _key(path)
        if not new_stars and new_label == "none":
            self.data.pop(key, None)
        else:
            self.data[key] = (new_stars, new_label)

    def save(self):
        if not self.path:
            return False
        payload = {"version": VERSION,
                   "ratings": {k: {"stars": v[0], "label": v[1]}
                               for k, v in self.data.items()}}
        return edits.write_json(self.path, payload)

    def clear(self):
        self.data = {}
        self.save()

    def count_rated(self, paths):
        """这批素材里评过级的有几张（状态栏用）。"""
        return sum(1 for p in paths if self.get(p) != (0, "none"))
