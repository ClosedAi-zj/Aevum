# -*- coding: utf-8 -*-
"""结果卡回归：一次运行的输出要分层，而且每个数字都能问出含义。

以前任务跑完只有一屏滚动的纯文本，"成没成、成品在哪、这数字算正常吗"
全得自己在几百行日志里翻。这里盯三件事：
① 日志确实搬进了卡片的折叠区 —— 而且模块的 log_view 还是原来那个，
   别的测试里 `log_view.toPlainText()` 的断言照旧成立；
② 跑完卡片上有结论、有数字，每个数字都带一句悬停解释；
③ 自动展开/收起日志不抢用户手里那个按钮。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_result.py
"""
import collections
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "acceptance"))

from PySide6 import QtGui, QtWidgets      # noqa: E402

from reference import motion                      # noqa: E402
from reference.gui import theme, widgets, workbench    # noqa: E402

# 用入口真正注册的那份模块表：测试跑的就是用户双击出来的那个工作台
from apps.workbench import MODULES      # noqa: E402

FAILS = []
TMP = os.path.join(HERE, "_tmp")
SHOTS = os.path.join(ROOT, "shots")
CARDS = {}                               # 结果卡 -> 模块名，报断言时用

# 伪造一次对齐的 summary，字段与 AlignJob.summary 一致
AL = {"n": 8, "dx": (-2.0, 1.5), "dy": (0.0, 3.0),
      "crop": (4, 5, 716, 535), "loss": 1.4, "issues": 0,
      "out": (712, 530), "rot": (-0.12, 0.20)}
AL_BAD = dict(AL, loss=26.0, issues=5)


def check(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}"
          + (f"  {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def _big_file(name):
    """造一个足够大的假成片：体积指标不至于把卡片判成"异常小"。"""
    os.makedirs(TMP, exist_ok=True)
    p = os.path.join(TMP, name)
    with open(p, "wb") as fh:
        fh.write(b"\0" * 300_000)
    return p


def _chips(card):
    return [c for c in card.box_metrics.findChildren(QtWidgets.QFrame)
            if c.objectName() == "Metric"]


def test_card_machine():
    """卡片自己的状态：折叠、自动展开的边界、指标解释的写法。"""
    card = widgets.ResultCard("测试卡")
    check("没有日志时折叠键不亮", not card.btn_more.isEnabled())
    log = QtWidgets.QPlainTextEdit()
    card.set_detail(log)
    check("日志住进折叠区", log.parent() is card.box_detail)
    check("平时是折起来的",
          not card.details_open() and not card.box_detail.isVisibleTo(card))
    check("未知结论类别不炸", card.set_status("??", "怪").status_kind() == "??")

    folded = card.sizeHint().height()
    # 用户嫌的就是这个：没跑过时卡片被拉成一大块空文本框。
    check("空着时只剩一条标题", folded <= 60, f"{folded}px")
    check("卡片不被布局拉高（纵向跟内容走）",
          card.sizePolicy().verticalPolicy()
          == QtWidgets.QSizePolicy.Policy.Maximum,
          str(card.sizePolicy().verticalPolicy()))
    card.set_status("run", "跑着")
    check("开跑时日志摊开",
          card.details_open() and card.box_detail.isVisibleTo(card))
    check("摊开日志卡片长出来", card.sizeHint().height() > folded,
          f"{folded}px → {card.sizeHint().height()}px")
    card.set_status("ok", "成了")
    check("出结论后自动收回", not card.details_open())
    check("收回后高度回到一条", card.sizeHint().height() == folded,
          f"{card.sizeHint().height()}px vs {folded}px")

    card.btn_more.click()                        # 用户自己摊开
    card.set_status("run", "又跑")
    card.set_status("ok", "又成")
    check("用户摊开的不会被自动收回", card.details_open())
    card.btn_more.click()                        # 用户又折起
    card.set_status("run", "再跑")
    check("每次开跑都摊开进度", card.details_open())
    card.set_status("ok", "再成")
    check("这一回不替用户收", not card.details_open())

    card.set_metrics([("面积损失", "12.4%"), ("帧数", "8", "参与帧数")])
    items = card.metric_items()
    check("指标按（键, 值, 解释）归一",
          items[0][2] is None and items[1][2] == "参与帧数", str(items))
    card.set_metrics([])
    check("空列表把指标行收掉", not card.box_metrics.isVisibleTo(card))

    # 步骤时间线：跑几步就画几行，只有一步时不占地方
    card.set_steps(["渲帧", "编码"])
    check("两步画成时间线",
          card.box_steps.isVisibleTo(card)
          and card.steps() == [("渲帧", "wait", ""), ("编码", "wait", "")])
    card.stage(0, "run")
    card.stage(0, "ok", "14 帧")
    card.stage(1, "run")
    check("状态按步走",
          card.steps() == [("渲帧", "ok", "14 帧"), ("编码", "run", "")],
          str(card.steps()))
    card.stage(1, "bad", "退出码 1")
    check("失败的那步标红", card.steps()[1][1] == "bad"
          and card.steps()[1][2] == "退出码 1", str(card.steps()))
    card.stage(9, "ok")
    check("越界的步号不炸", len(card.steps()) == 2)
    card.set_steps(["只此一步"])
    check("单步任务不画时间线", not card.box_steps.isVisibleTo(card),
          str(card.steps()))
    check("行数跟着步数缩", len(card._rows) == 2
          and not card._rows[1].isVisibleTo(card))

    card.set_path("D:/Windows", "成品：")
    check("没有成品时不打开放键", card.set_path(None) is card
          and not card.btn_open.isVisibleTo(card))
    card.deleteLater()
    log.deleteLater()


def _filled(card, kind, head):
    """结论 + 数字都到位，而且每个数字都能问出含义。"""
    nm = CARDS.get(card, "结果卡")
    check(f"{nm}：结论开头写着「{head}」",
          card.lb_title.text().startswith(head), card.lb_title.text())
    check(f"{nm}：结论类别是 {kind}", card.status_kind() == kind,
          card.status_kind())
    items = card.metric_items()
    check(f"{nm}：有数字可看", len(items) >= 4, f"{len(items)} 个")
    no_tip = [k for k, _v, t in items if not t]
    check(f"{nm}：每个数字都有悬停解释", not no_tip, "、".join(no_tip))
    thin = [k for k, _v, t in items if t and len(t) < 12]
    check(f"{nm}：解释不是敷衍一句", not thin, "、".join(thin))
    chips = _chips(card)
    check(f"{nm}：数字画成了小卡", len(chips) == len(items),
          f"{len(chips)}/{len(items)}")
    bare = [c for c in chips if not c.toolTip()]
    check(f"{nm}：小卡本身也能悬停", not bare, f"{len(bare)} 个没有")
    tip = card.btn_mark.toolTip()
    shown = [w for w in card.findChildren(QtWidgets.QLabel)
             if w.text() and w.text() == card.next_hint()]
    check(f"{nm}：下一步只写在悬停里",
          bool(card.next_hint()) and card.next_hint() in tip and not shown,
          tip.replace("\n", " / "))


def _shoot(wb, name):
    p = os.path.join(SHOTS, f"shot_{name}.png")
    wb.grab().save(p)
    return p, os.path.getsize(p)


def _ink(widget):
    """控件抓图里"和底色不一样"的像素数——判断真画出了东西。

    底色取众数（每 7 个像素采一个，卡片绝大部分是平的），再逐像素比对。
    """
    img = widget.grab().toImage().convertToFormat(
        QtGui.QImage.Format.Format_ARGB32)
    px = bytes(img.constBits())
    n = len(px) // 4
    seen = collections.Counter(px[i * 4:i * 4 + 4] for i in range(0, n, 7))
    bg = seen.most_common(1)[0][0]
    return sum(1 for i in range(n) if px[i * 4:i * 4 + 4] != bg)


def _module_card(wb, name):
    """每个模块都得有一张结果卡，而且日志确实归它管。"""
    mod = wb.module(name)
    cards = wb.page_of[name].findChildren(widgets.ResultCard)
    check(f"{name}：页面上有一张结果卡", len(cards) == 1, f"{len(cards)} 张")
    ok = (getattr(mod, "result", None) in cards
          and mod.log_view.parent() is mod.result.box_detail)
    check(f"{name}：日志搬进了卡片的折叠区", ok,
          type(mod.log_view.parent()).__name__)
    CARDS[mod.result] = name
    return mod


def test_modules(app):
    wb = workbench.Workbench(MODULES, restore=False)
    wb.show()
    app.processEvents()

    al = _module_card(wb, "对齐")
    check("对齐：卡片起手没有数字", _chips(al.result) == [])
    # 空状态先拍：这一张就是"卡片收成一条、不再是一大块空文本框"的证据
    wb.set_module("对齐")
    app.processEvents()
    _path, size = _shoot(wb, "结果卡_空状态")
    check("空状态卡片薄成一条", 20 < al.result.height() <= 60,
          f"{al.result.height()}px / 整窗 {size // 1024}KB")
    al._show_summary({})
    check("对齐：换目录回到初始结论",
          al.result.status_kind() == "idle" and not al.result.metric_items(),
          al.result.lb_title.text())
    al._show_summary(AL, "probe")
    check("对齐：预检说清未写文件",
          "未写文件" in al.result.lb_title.text(), al.result.lb_title.text())
    al._show_summary(AL)
    _filled(al.result, "ok", "对齐完成")
    check("对齐：位移范围进了数字里",
          any("px" in v for _k, v, _t in al.result.metric_items()))
    # 时间线出图：状态平时由任务线程的 stage 信号一步步推，这里按同一个
    # 顺序摆一遍，只为让 shots 里留下一张看得清的样子（接线由 test_motion 验）
    al.result.set_steps(["测位移", "平滑与裁切", "写出序列", "抽帧校验"])
    al.result.stage(0, "ok", "3.1s 异常 0 帧")
    al.result.stage(1, "ok", "裁到 3840×2160")
    al.result.stage(2, "run", "")
    # 卡片只在所在页被激活后才有真实宽度：没显示过的页面上它还是初始那点尺寸，
    # 拍出来的小卡数字全被挤没，看着像坏了。
    wb.set_module("对齐")
    app.processEvents()
    p = os.path.join(SHOTS, "shot_结果卡时间线.png")
    al.result.grab().save(p)
    check("时间线画得出来", os.path.getsize(p) > 4000
          and _ink(al.result.box_steps) > 200 and al.result.width() > 400,
          f"{os.path.getsize(p) // 1024}KB {al.result.width()}px 时间线上墨 "
          f"{_ink(al.result.box_steps)}")
    al.result.set_steps([])
    al._show_summary(AL_BAD)
    _filled(al.result, "warn", "对齐完成")
    check("对齐：裁得太多会提醒", "异常" in al.result.next_hint()
          or "重跑" in al.result.next_hint(), al.result.next_hint())
    # 这句话不在卡面上——只挂在状态点和标题的悬停里
    check("对齐：提醒只在悬停里，不占卡面",
          all(w.text() != al.result.next_hint()
              for w in al.result.findChildren(QtWidgets.QLabel) if w.text()),
          al.result.btn_mark.toolTip().replace("\n", " / "))

    asm = _module_card(wb, "合成")
    asm._run_info = {"fps": 60, "frames": 1200, "codec": "hevc", "crf": 18,
                     "gaps": 0, "dims": [("a.mp4", "3840×2160")]}
    asm._show_result([_big_file("_result_asm.mp4")])
    _filled(asm.result, "ok", "合成完成")
    check("合成：时长算得出来",
          any(v == "20.0 秒" for _k, v, _t in asm.result.metric_items()),
          str(asm.result.metric_items()))
    check("合成：成品路径可点",
          os.path.basename(asm.result._path) == "_result_asm.mp4",
          str(asm.result._path))

    cr = _module_card(wb, "裁剪")
    cr._run_info = {"w": 3840, "h": 2160, "W": 4096, "H": 2160,
                    "desc": "比特流无损（只改写码流裁切窗口）", "dur": 6.0}
    cr._show_crop(_big_file("_result_crop.mp4"))
    _filled(cr.result, "ok", "裁剪完成")
    check("裁剪：算出裁掉了多少",
          ("裁掉", "6.2%") in [(k, v) for k, v, _t in cr.result.metric_items()],
          str(cr.result.metric_items()))

    au = _module_card(wb, "配乐")
    au._run_info = {"opt": {"mode": "replace", "gain": 100, "delay": 0.0,
                            "fade_in": 1.0, "fade_out": 1.5, "loop": True,
                            "trim": True, "orig_gain": 0},
                    "dur": 24.0, "vd": 20.0, "ad": 12.0, "mode": "换成配乐"}
    au._show_result(_big_file("_result_mix.mp4"))
    _filled(au.result, "ok", "配乐完成")
    check("配乐：时长算式进了数字",
          any(v == "24.0s" for _k, v, _t in au.result.metric_items()),
          str(au.result.metric_items()))

    rt = _module_card(wb, "运动")
    rt._run_info = {"w": 1920, "h": 1080, "n": 300, "src_n": 600, "fps": 30.0,
                    "total": 10.0, "avg": 2.0, "rate": 2.0, "curve": False,
                    "mode": motion.MIX, "taps": 5, "strength": 0.7,
                    "audio": True, "filter": "atempo=2", "kind": "video"}
    rt._show(_big_file("_result_rt.mp4"))
    _filled(rt.result, "ok", "重定时完成")
    check("运动：帧数算式进了数字",
          any(v == "600 → 300" for _k, v, _t in rt.result.metric_items()),
          str(rt.result.metric_items()))
    check("运动：音轨说清怎么跟着变",
          any(_k == "音轨" and "atempo" in v
              for _k, v, _t in rt.result.metric_items()))
    # 曲线变速时声音跟不上画面，这件事只能由结果卡来说
    rt._run_info = dict(rt._run_info, curve=True, filter=None, rate=0.5,
                        avg=0.5)
    rt._show(_big_file("_result_rt.mp4"))
    check("运动：曲线变速会提醒音画逐渐错位",
          rt.result.status_kind() == "warn"
          and "对不上" in rt.result.btn_mark.toolTip(),
          rt.result.btn_mark.toolTip())
    check("运动：音轨那格写了只能保持原速",
          any(_k == "音轨" and "原速" in v
              for _k, v, _t in rt.result.metric_items()),
          str(rt.result.metric_items()))
    # 慢放到 0.5× 却一种混合都不开：成片会一顿一顿
    rt._run_info = dict(rt._run_info, curve=False, mode=motion.OFF)
    rt._show(_big_file("_result_rt.mp4"))
    check("运动：慢放不混合会提醒换邻帧混合",
          rt.result.status_kind() == "warn"
          and "邻帧混合" in rt.result.btn_mark.toolTip(),
          rt.result.btn_mark.toolTip())

    # 卡片不能把日志永久摊开：左栏寸土寸金
    for nm in ("对齐", "合成", "裁剪", "配乐", "运动"):
        c = wb.module(nm).result
        check(f"{nm}：静止时折叠区收起", not c.details_open(),
              c.lb_title.text())

    # 截图由测试生成：五个模块的结果卡都填好数字之后再拍。
    # 判"有没有真内容"数像素，不看 PNG 字节：合成页中间那块画布是空的，
    # 整窗截图压出来就特别小（94KB），拿体积当阈值会假报警。
    for nm in ("对齐", "合成", "裁剪", "配乐", "运动"):
        wb.set_module(nm)
        app.processEvents()
        _path, size = _shoot(wb, f"结果卡_{nm}")
        card = wb.module(nm).result
        ink = _ink(card)
        check(f"{nm}：结果卡真的画出来了", ink > 3000,
              f"{ink} 个非底色像素 / 整窗 {size // 1024}KB")
        # 用户点的就是这儿：卡片跟着左栏吃满剩余高度，没内容时也撑成
        # 一个空文本框。现在纵向跟 sizeHint 走，多出来的高度还给画布。
        check(f"{nm}：卡片只占内容那么高",
              card.height() <= card.sizeHint().height() + 2,
              f"{card.height()}px vs 内容 {card.sizeHint().height()}px")

    # 摊开日志的那一档也要拍：折叠区得把留白吃掉，而不是挤成一溜
    wb.set_module("对齐")
    al.log_view.appendPlainText("[00:01] 参考帧 PANA0001.JPG")
    al.log_view.appendPlainText("[00:02] 位移 dx=-2.0~1.5 dy=0.0~3.0")
    al.log_view.appendPlainText("[00:03] 统一裁切 (4,5)-(716,535)")
    app.processEvents()
    folded = al.result.height()
    al.result.btn_more.click()
    app.processEvents()
    check("展开后折叠区占住高度",
          al.result.box_detail.height() > 60 and al.result.height() > folded,
          f"折叠区 {al.result.box_detail.height()}px，"
          f"卡片 {folded}px → {al.result.height()}px")
    _path, size = _shoot(wb, "结果卡_展开日志")
    check("展开日志截图", size > 100000, f"{size // 1024}KB")

    wb.close()
    app.processEvents()


def main():
    app = QtWidgets.QApplication.instance() or QtWidgets.QApplication(sys.argv)
    theme.apply(app)
    test_card_machine()
    test_modules(app)
    print("\n全部通过" if not FAILS else
          f"\n失败 {len(FAILS)} 项：{'、'.join(FAILS)}")
    sys.exit(1 if FAILS else 0)


if __name__ == "__main__":
    main()
