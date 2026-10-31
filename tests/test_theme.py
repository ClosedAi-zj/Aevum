# -*- coding: utf-8 -*-
"""主题层回归：token 完整性、扁平约束、文字/底色对比度、窗口弧线、字号四档。

皮肤只是"一张数值表 + 一份共用 QSS"，最容易出的两类事故：
新增皮肤漏 token（会在 _build_qss 里直接 KeyError），或者颜色配得看不清
（只有算 WCAG 对比度能发现，肉眼看截图永远觉得"还行"）。
窗口弧线是原生 DWM 属性，抓图抓不到，所以钉的是 API 返回值。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_theme.py
"""
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "acceptance"))

from PySide6 import QtWidgets         # noqa: E402

from reference.gui import theme           # noqa: E402
from reference.gui import widgets         # noqa: E402

FAILS = []


def check(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}"
          + (f"  {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def test_tokens():
    names = set(theme._token_names)
    holes = {k: sorted(names - set(v)) for k, v in theme._SKIN_BASE.items()}
    check("每套皮肤 token 齐全", not [h for h in holes.values() if h],
          "、".join(f"{k}:{','.join(h)}" for k, h in holes.items() if h))
    extra = {k: sorted(set(v) - names - {"label"})
             for k, v in theme._SKIN_BASE.items()}
    check("没有皮肤自带野 token", not [e for e in extra.values() if e])
    check("皮肤标签与键一一对应",
          all(theme.label(k) != k and "dark" in v for k, v in
              theme._SKIN_BASE.items()))
    darks = [k for k in theme.SKINS if theme._SKIN_BASE[k]["dark"]]
    lights = [k for k in theme.SKINS if not theme._SKIN_BASE[k]["dark"]]
    check("深色与浅色都有得选", len(darks) >= 3 and len(lights) >= 2,
          f"深 {len(darks)} 套、浅 {len(lights)} 套")
    check("底色互不重复",
          len({v["BG"] for v in theme._SKIN_BASE.values()}) == len(theme.SKINS))
    # dark 标错就全乱：标题栏、调色板、图标都按它决定往亮还是往暗走
    wrong = []
    for k, v in theme._SKIN_BASE.items():
        lum = theme._lum(theme._rgba(v["BG"]))
        if v["dark"] != (lum < 0.25):
            wrong.append(f"{k}(BG 亮度 {lum:.3f}, dark={v['dark']})")
    check("深浅标记与底色一致", not wrong, "、".join(wrong))


def test_flat():
    grads, shadows, missing, bad_radii = [], [], [], set()
    allowed = {theme.R_SM, theme.R_MD, theme.R_LG, theme.R_XL,
               theme.R_PILL, 2, 4}
    for k in theme.SKINS:
        theme.apply_tokens(k)
        qss = theme.QSS
        if "gradient" in qss.lower():
            grads.append(k)
        if "box-shadow" in qss.lower():
            shadows.append(k)
        for num in re.findall(r"border-radius:\s*(\d+)px", qss):
            if int(num) not in allowed:
                bad_radii.add(int(num))
        # token 真的流进 QSS 了才算这套皮肤生效
        if theme._SKIN_BASE[k]["BG"] not in qss:
            missing.append(k)
    check("全界面没有渐变", not grads, "、".join(grads))
    check("全界面没有阴影", not shadows, "、".join(shadows))
    check("圆角只用统一尺度", not bad_radii, str(sorted(bad_radii)))
    check("每套皮肤都生成了 QSS", not missing, "、".join(missing))
    theme.apply_tokens(theme.DEFAULT_SKIN)


def test_contrast():
    bad = theme.audit()
    check("所有皮肤的字体与底色达标", not bad,
          "、".join(f"{k} {where} {got}<{need}" for k, where, got, need in bad))
    # 单独钉最要读的两处：正文和强调色按钮上的字
    worst_text = min(theme.contrast(v["TEXT"], v["BG"])
                     for v in theme._SKIN_BASE.values())
    worst_button = min(theme.contrast(v["ON_ACCENT"], v["ACCENT"])
                       for v in theme._SKIN_BASE.values())
    check("正文对比度 ≥4.5", worst_text >= 4.5, f"最低 {worst_text:.2f}")
    check("按钮文字对比度 ≥4.5", worst_button >= 4.5,
          f"最低 {worst_button:.2f}")
    # 探针：同一对颜色不该给出两个比值，半透明要先合成
    check("半透明按合成后算",
          theme.contrast("rgba(0, 0, 0, 0.5)", "#ffffff")
          == theme.contrast("#808080", "#ffffff"))
    check("黑白对比是 21", abs(theme.contrast("#000000", "#ffffff") - 21) < 0.01)


def test_corner():
    check("圆角档位齐全",
          [m for _l, m in theme.CORNER_MODES] == [-1, 2, 3, 1],
          str(theme.CORNER_MODES))
    check("档位有人话标签",
          all(theme.corner_label(m) for _l, m in theme.CORNER_MODES)
          and theme.corner_label(99) == "跟随系统")
    theme.set_corner_mode(-1)
    check("跟随系统时不干预", theme.CORNER == -1)
    theme.set_corner_mode(2)
    if sys.platform != "win32":
        print("SKIP  非 Windows，跳过 DWM 圆角实测")
        return
    w = QtWidgets.QMainWindow()
    w.resize(240, 160)
    w.show()
    app = QtWidgets.QApplication.instance()
    app.processEvents()
    for mode, name in ((2, "大圆角"), (3, "小圆角"), (1, "直角")):
        theme.set_corner_mode(mode)
        check(f"DWM 接受{name}", theme.CORNER_HR == 0,
              f"hr=0x{theme.CORNER_HR & 0xFFFFFFFF:08x}")
    theme.set_corner_mode(2)
    w.deleteLater()


def test_font(app):
    """字号四档：每一处字号都得跟着档位走，且全用 px。

    以前 QSS 写死 15px 正文、自绘文字用 9pt：pt 在 Windows 上还要再乘一次
    DPI，同一屏里就出现"标题还没正文大"的怪比例。现在所有字号过 theme.fs()。
    """
    keys = [k for k, _s in theme.FONT_LEVELS]
    check("四档、标准档不动", keys == ["紧凑", "标准", "宽松", "特大"]
          and dict(theme.FONT_LEVELS)["标准"] == 1.0, str(keys))
    check("QSS 里不许混 pt 字号",
          not re.search(r"font-size:\s*[\d.]+pt", theme.QSS))
    peaks = []
    smallest = 99
    ok_px = True
    for key in keys:
        theme.set_font(app, key)
        sizes = [int(m) for m in re.findall(r"font-size: (\d+)px", theme.QSS)]
        peaks.append(max(sizes))
        smallest = min(smallest, min(sizes))
        ok_px &= (app.font().pointSize() == -1
                  and app.font().pixelSize() == theme.fs(theme.FONT_BASE))
    check("四档字号逐级变大", all(a < b for a, b in zip(peaks, peaks[1:])),
          " → ".join(str(p) for p in peaks))
    check("最小的字也不许小到没法读", smallest >= 10, f"最小 {smallest}px")
    check("应用字体按 px 走", ok_px,
          f"{app.font().pixelSize()}px  pointSize={app.font().pointSize()}")
    # 认不出的档位要退回标准，别让设置里写脏一次就把界面搞崩
    check("坏档位退回标准", theme.set_font(app, "超大") == theme.DEFAULT_FONT,
          theme.FONT)
    g = widgets.Group("折叠高度")
    check("分组不吸走面板剩余高度",
          g.sizePolicy().verticalPolicy() == QtWidgets.QSizePolicy.Policy.Fixed,
          str(g.sizePolicy().verticalPolicy()))
    g.deleteLater()
    # 推子的行高是 setFixedHeight 钉住的，换档时要靠 theme.on_change 补一次
    sl = widgets.Slider()
    sl.show()
    app.processEvents()
    theme.set_font(app, "特大")
    big = sl.height()
    want = theme.fs(widgets.Slider.ROW)
    theme.set_font(app, "标准")
    check("换字号档推子行高跟着变",
          big == want and want != 22 and sl.height() == 22,
          f"特大 {big}px / 标准 {sl.height()}px")
    sl.deleteLater()


def test_hot_swap(app):
    w = QtWidgets.QMainWindow()
    w.resize(400, 300)
    w.show()
    app.processEvents()
    ok_palette = ok_qss = ok_icon_role = True
    for key in theme.SKINS:
        theme.set_skin(app, key)
        app.processEvents()
        T = theme.TOK
        ok_palette &= app.palette().window().color().name().upper() \
            == T["BG"].upper()
        ok_qss &= T["BG"] in theme.QSS and theme.SKIN == key
        ok_icon_role &= all(tok in T for tok in theme.ICON_ROLES.values())
    check("换肤同步调色板", ok_palette)
    check("换肤重建 QSS", ok_qss)
    check("图标角色取得到色", ok_icon_role)
    check("窗口弧线在换肤后仍然生效",
          theme.set_corner_mode(2) == 2 and (
              sys.platform != "win32" or theme.CORNER_HR == 0))
    theme.set_skin(app, theme.DEFAULT_SKIN)
    w.deleteLater()


def main():
    app = QtWidgets.QApplication(sys.argv)
    theme.apply(app)
    test_tokens()
    test_flat()
    test_contrast()
    test_corner()
    test_font(app)
    test_hot_swap(app)
    print("\n全部通过" if not FAILS else
          f"\n失败 {len(FAILS)} 项：{'、'.join(FAILS)}")
    sys.exit(1 if FAILS else 0)


if __name__ == "__main__":
    main()
