# -*- coding: utf-8 -*-
"""参数讲解层回归：文案表本身，以及它是否真的挂到了界面上。

写清楚"这个旋钮往哪边调会怎样"是这层唯一的目的。最容易悄悄退化的两件事：
① 文案加了却没人挂（写了个寂寞）；② 界面改版把某个控件的讲解弄丢了。
所以既查文案表结构，也真的把六个模块建出来数 tooltip。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_hints.py
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, ROOT)

from PySide6 import QtWidgets           # noqa: E402

from lapse.gui import hints, theme, widgets, workbench   # noqa: E402
from lapse.gui import settingsdialog    # noqa: E402

# 用入口真正注册的那份模块表：测试跑的就是用户双击出来的那个工作台
from apps.workbench import MODULES      # noqa: E402

FAILS = []
# 每个模块至少该有几处带【标题】的完整讲解 / 几行常驻短说明
MIN = {"对齐": (10, 4), "合成": (7, 4), "裁剪": (6, 1), "配乐": (8, 4),
       "运动": (12, 6)}


def check(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}"
          + (f"  {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def _gui_src():
    out = []
    for base, _dirs, files in os.walk(os.path.join(ROOT, "lapse", "gui")):
        for f in files:
            if f.endswith(".py"):
                with open(os.path.join(base, f), encoding="utf-8") as fh:
                    out.append(fh.read())
    return "\n".join(out)


def test_table():
    bad = [k for k, t in hints.TOPICS.items()
           if not all(t.get(f) for f in ("title", "line", "what"))]
    check("每条讲解都有标题/短行/正文", not bad, "、".join(bad))
    long = [k for k, t in hints.TOPICS.items() if len(t["line"]) > 22]
    check("常驻短行够短（检查器只有 374px 宽）", not long,
          "、".join(f"{k}({len(hints.TOPICS[k]['line'])})" for k in long))
    nopfx = [k for k in hints.TOPICS if "/" not in k]
    check("键都带模块前缀", not nopfx, "、".join(nopfx))
    src = _gui_src()
    orphan = [k for k in hints.TOPICS if f'"{k}"' not in src]
    check("没有写了却没挂的文案", not orphan, "、".join(orphan))


def test_api():
    try:
        hints.line("没这个键")
        check("未知键要报错", False)
    except KeyError as e:
        check("未知键要报错", "没有这条参数讲解" in str(e), str(e))
    t = hints.text("对齐/搜索余量", current="范围 0~500，默认 150")
    check("tooltip 全文分段齐全",
          t.startswith("【搜索余量】") and "怎么调：" in t and "注意：" in t
          and t.endswith("范围 0~500，默认 150"), t.replace("\n", " / "))
    check("没有注意事项时不硬凑那行", "注意：" not in hints.text("对齐/抽帧"))


def test_field_block():
    box = widgets.field_block("帧率", QtWidgets.QComboBox(),
                              hint=hints.line("合成/帧率"))
    lb = box.hint_label
    check("field_block 能挂常驻短行",
          lb is not None and lb.objectName() == "FieldHint"
          and lb.text() == hints.line("合成/帧率") and lb.wordWrap(),
          lb.text() if lb else "没有")
    plain = widgets.field_block("帧率", QtWidgets.QComboBox())
    check("不传 hint 时不多出一行",
          plain.hint_label is None and plain.layout().count() == 2)


def test_attached(app):
    wb = workbench.Workbench(MODULES, restore=False)
    wb.show()
    app.processEvents()
    for name, (min_tips, min_lines) in MIN.items():
        page = wb.page_of[name]
        tips = len([w for w in page.findChildren(QtWidgets.QWidget)
                    if w.toolTip().startswith("【")])
        lines = len([w for w in page.findChildren(QtWidgets.QLabel)
                     if w.objectName() == "FieldHint" and w.text()])
        check(f"{name}：控件挂了完整讲解", tips >= min_tips,
              f"{tips} 处（至少 {min_tips}）")
        check(f"{name}：参数下面有常驻短行", lines >= min_lines,
              f"{lines} 行（至少 {min_lines}）")
    for name in ("对齐", "合成", "裁剪", "配乐", "运动"):
        check(f"{name}：顶栏那句由自己给", "top_hint" in vars(type(wb.module(name))),
              type(wb.module(name)).__name__)
    # 顶栏那句得真的跟着状态变，而不是永远空着
    al = wb.module("对齐")
    al.files = ["a.jpg"] * 12
    al._stage = "已预检"
    check("顶栏那句跟着状态走", al.top_hint() == "12 张 · 已预检", al.top_hint())
    al._stage = ""
    check("没走到那一步就只报张数", al.top_hint() == "12 张", al.top_hint())
    al.files = None
    check("没素材时顶栏不硬凑", al.top_hint() == "", al.top_hint())
    asm = wb.module("合成")
    asm.series = {"count": 1386}
    check("合成顶栏报张数", asm.top_hint() == "1386 张", asm.top_hint())
    asm.series = None
    cr = wb.module("裁剪")
    cr.info = {"image": True, "w": 640, "h": 420, "dur": 0.0}
    check("裁剪顶栏报素材规格", cr.top_hint() == "640×420", cr.top_hint())
    cr.info = None
    dlg = settingsdialog.SettingsDialog(wb)
    tips = len([w for w in dlg.findChildren(QtWidgets.QWidget)
                if w.toolTip().startswith("【")])
    check("设置：每项都有讲解", tips >= 6, f"{tips} 处")
    dlg.deleteLater()
    wb.close()
    app.processEvents()


def main():
    app = QtWidgets.QApplication.instance() or QtWidgets.QApplication(sys.argv)
    theme.apply(app)
    test_table()
    test_api()
    test_field_block()
    test_attached(app)
    print("\n全部通过" if not FAILS else
          f"\n失败 {len(FAILS)} 项：{'、'.join(FAILS)}")
    sys.exit(1 if FAILS else 0)


if __name__ == "__main__":
    main()
