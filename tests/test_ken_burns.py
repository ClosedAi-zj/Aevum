# -*- coding: utf-8 -*-
"""运镜（Ken Burns）：取景框插值单测 + 图片/视频端到端出片。

几何与重画都在 acceptance/reference/crop.py，界面这边要钉的是"参数真的进到成片里"：
帧数、成片尺寸、推近确实裁掉了边、视频音轨跟着过来、取消不留中间帧目录。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_ken_burns.py
"""
import os
import shutil
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "acceptance"))
TMP = os.path.join(HERE, "_tmp_kb")
SHOTS = os.path.join(ROOT, "shots")
os.makedirs(TMP, exist_ok=True)

import cv2                                     # noqa: E402
import numpy as np                             # noqa: E402
from PySide6 import QtCore, QtWidgets          # noqa: E402

from reference import crop as cp                   # noqa: E402
from reference import video as vmod                # noqa: E402
from reference.ff import (find_ffmpeg, probe_audio, probe_video,   # noqa: E402
                      run as ff_run)
from reference.gui import cropper, theme, workbench                # noqa: E402

FF = find_ffmpeg()
STILL = os.path.join(TMP, "_kb_still.png")
MOVIE = os.path.join(TMP, "_kb_src.mp4")
FAILS = []


def check(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}"
          + (f"  {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def pump(app, ms):
    t0 = time.time()
    while (time.time() - t0) * 1000 < ms:
        app.processEvents()
        QtCore.QThread.msleep(5)
    app.processEvents()


def wait_job(app, wb, timeout=240):
    t0 = time.time()
    while wb._job is not None and wb._job.isRunning() and time.time() - t0 < timeout:
        pump(app, 60)
    pump(app, 250)
    return wb._job is None or not wb._job.isRunning()


def frame(path, t):
    out = os.path.join(TMP, "_kb_f.png")
    if os.path.exists(out):
        os.remove(out)
    ff_run([FF, "-y", "-hide_banner", "-loglevel", "error", "-ss", f"{t}",
            "-i", path, "-frames:v", "1", out])
    return cv2.imread(out)


def near(a, b, tol=1.0):
    return abs(a - b) <= tol


# ---------------------------------------------------------------- 几何
def test_geometry():
    a = (0.0, 0.0, 640.0, 360.0)
    b = (64.0, 36.0, 512.0, 288.0)
    check("缓动两端不动",
          near(cp.kb_ease(0.0), 0.0) and near(cp.kb_ease(1.0), 1.0)
          and near(cp.kb_ease(0.5, "linear"), 0.5))
    check("缓入缓出中点是中点", near(cp.kb_ease(0.5), 0.5))
    check("起步慢真的慢", all(cp.kb_ease(t, "in") < t for t in (0.2, 0.5, 0.8)))
    check("收尾慢真的快起步", all(cp.kb_ease(t, "out") > t for t in (0.2, 0.5, 0.8)))
    check("缓动越界夹住", near(cp.kb_ease(-3.0), 0.0) and near(cp.kb_ease(9.0), 1.0))

    rs = cp.kb_rects(a, b, 5, "smooth")
    check("帧数对得上", len(rs) == 5)
    check("第一帧就是起点", all(near(u, v) for u, v in zip(rs[0], a)), str(rs[0]))
    check("最后一帧就是终点", all(near(u, v) for u, v in zip(rs[-1], b)), str(rs[-1]))
    check("单调推进不出界", all(r[2] <= a[2] + 1e-9 and r[0] >= -1e-9
                                for r in rs))
    mid = rs[2]
    check("中点落在两端之间",
          mid[2] < rs[1][2] and mid[2] > rs[3][2] and near(mid[2], 576.0),
          str([round(v, 2) for v in mid]))

    one = cp.kb_rects(a, b, 1)
    check("单帧停在起点", len(one) == 1 and near(one[0][2], a[2]), str(one))

    bad = []
    for r in cp.kb_rects(a, b, 40, "smooth") + cp.kb_rects(a, b, 40, "linear"):
        if not near(r[2] / r[3], 16 / 9, 0.02) or r[0] < -1e-6 or \
                r[0] + r[2] > 640.0001 or r[1] + r[3] > 360.0001:
            bad.append(str([round(v, 2) for v in r]))
    check("逐帧锁比例且始终在画面内", not bad, "; ".join(bad[:3]))

    w, h = cp.kb_size(b, 3840)
    check("成片长边给足且宽高取偶", w == 3840 and h % 2 == 0 and h == 2160,
          f"{w}x{h}")
    check("竖框时竖着给长边", cp.kb_size((0, 0, 90, 160), 1080) == (606, 1080),
          str(cp.kb_size((0, 0, 90, 160), 1080)))
    check("非法取景框报错", _raises(ValueError, cp.kb_size, (0, 0, 0, 100), 640))
    check("推近时倍率大于 1", cp.kb_zoom(b, w, h) > cp.kb_zoom(a, w, h),
          f"{cp.kb_zoom(a, w, h):.2f} -> {cp.kb_zoom(b, w, h):.2f}")

    # 一张左右渐变图：推近必然把左右两边吃掉，最容易验证方向
    img = np.repeat(np.linspace(0, 255, 640)[None, :], 360, 0).astype(np.uint8)
    f0 = cp.kb_frame(img, a, 640, 360)
    f1 = cp.kb_frame(img, b, 640, 360)
    check("重画尺寸正确", f0.shape == (360, 640) and f1.shape == (360, 640))
    check("起点帧等于整幅", float(np.abs(f0.astype(int) - img).mean()) < 3.0,
          f"{np.abs(f0.astype(int) - img).mean():.2f}")
    check("推近真的裁掉了左右",
          f1[:, 0].mean() > f0[:, 0].mean() + 8 and
          f1[:, -1].mean() < f0[:, -1].mean() - 8,
          f"左边 {f0[:, 0].mean():.1f}->{f1[:, 0].mean():.1f}  "
          f"右边 {f0[:, -1].mean():.1f}->{f1[:, -1].mean():.1f}")
    # 缩小（框比输出画幅大）走整数裁切 + INTER_AREA，不能起摩尔纹
    f2 = cp.kb_frame(img, (0, 0, 640, 360), 160, 90)
    check("缩小时也等比取样", f2.shape == (90, 160) and
          abs(f2[:, -1].mean() - 250) < 12, f"{f2[:, -1].mean():.1f}")
    # 4px 条纹对相位最敏感：整像素裁切会让 0.4px 的位移完全没痕迹，
    # 而运镜每一帧都要在亚像素上走，所以这里必须看得出差别
    stripe = np.repeat((((np.arange(640)[None, :] // 4) % 2) * 255)
                       .astype(np.uint8), 360, 0)
    fa = cp.kb_frame(stripe, (160, 0, 320, 180), 320, 180)
    fb = cp.kb_frame(stripe, (160.4, 0, 320, 180), 320, 180)
    same = cp.kb_frame(stripe, (160, 0, 320, 180), 320, 180)
    check("亚像素位移不被抹平成整帧",
          int((fa != fb).sum()) > 1000 and int((fa != same).sum()) == 0,
          f"0.4px 差 {int((fa != fb).sum())} 像素，"
          f"同一个框重复画差 {int((fa != same).sum())} 像素")


def _raises(exc, fn, *args):
    try:
        fn(*args)
    except exc:
        return True
    except Exception:
        return False
    return False


# ---------------------------------------------------------------- 命令
def test_cmd():
    cmd, w, h = vmod.build_cmd(
        "ff", "frames", "out.mp4", 30, None, 640, 17, "medium", "hevc",
        "kb%05d.jpg", 0, 12, 640, 360, 0, False, progress=True,
        audio_src="src.mp4")
    s = " ".join(cmd)
    check("运镜编码命令带帧序列", "-i frames" + os.sep + "kb%05d.jpg" in s, s[-160:])
    check("音轨另接一路并只复制",
          "-i src.mp4 -map 0:v:0 -map 1:a? -c:a copy" in s, s)
    check("尺寸按取景框不裁切", f"scale={w}:{h}" in s and (w, h) == (640, 360), s)
    cmd2, _w, _h = vmod.build_cmd(
        "ff", "f", "o.mp4", 24, None, 320, 17, "medium", "h264",
        "kb%05d.jpg", 0, 5, 320, 180, 0, False, progress=True)
    check("图片运镜不接音轨", "1:a?" not in " ".join(cmd2), " ".join(cmd2))


# ---------------------------------------------------------------- 素材
def make_src():
    if FF is None:
        return False
    if not os.path.exists(STILL):
        # 左右渐变 + 一个白圆：既能看方向，也能看它确实被裁掉了
        x = np.linspace(0, 255, 960)[None, :, None]
        img = np.repeat(x.astype(np.uint8), 540, 0)
        img = np.dstack([img] * 3)
        cv2.circle(img, (700, 300), 70, (255, 255, 255), -1)
        cv2.imwrite(STILL, img)
    r = ff_run([FF, "-y", "-hide_banner", "-loglevel", "error",
                "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25:duration=1.2",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=1.2",
                "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                "-c:a", "aac", "-shortest", MOVIE])
    return os.path.exists(STILL) and r.returncode == 0 and os.path.exists(MOVIE)


# ---------------------------------------------------------------- 界面
def _setup(app, mods):
    wb = workbench.Workbench(mods)
    wb.resize(1480, 880)
    wb.show()
    pump(app, 200)
    return wb


def _warns():
    """QMessageBox.warning 是模态的，测试里必须换成记录文本，否则 pump 会卡死。"""
    got = []
    QtWidgets.QMessageBox.warning = staticmethod(
        lambda *a, **k: got.append(a[2] if len(a) > 2 else a[-1]))
    return got


def _kb_on(c, side="320", fps="10", sec=0.6):
    c.ck_kb.setChecked(True)
    c.cb_kb_side.setCurrentText(side)
    c.cb_kb_fps.setCurrentText(fps)
    c.sp_kb_sec.setValue(sec)
    i = c.kb_cb_preset.findData("ultrafast")
    if i >= 0:
        c.kb_cb_preset.setCurrentIndex(i)
    c.cb_kb_codec.setCurrentText("H.264 / x264（兼容性优先）")


def test_image_gui(app):
    wb = _setup(app, [cropper.Cropper])
    c = wb.module(cropper.Cropper.name)
    warns = _warns()

    c.load(STILL)
    pump(app, 400)
    check("图片已装载", c.info is not None and c.info["w"] == 960, c.lb_info.text())
    check("默认没开运镜", not c._kb_on() and not c.kb_body.isVisible())
    check("图片本来就没有输出模式", not c.box_mode.isVisible()
          and c.btn_run.text() == "裁剪并另存", c.btn_run.text())

    c.ck_kb.setChecked(True)
    pump(app, 120)
    check("勾选后参数组长出来", c.kb_body.isVisible())
    check("运镜说明写在输出栏", "逐帧重画" in c.lb_out_note.text(),
          c.lb_out_note.text())
    check("按钮改口", c.btn_run.text() == "生成运镜成片", c.btn_run.text())
    check("自由比例被禁用", not c.cb_aspect.model().item(0).isEnabled())
    check("比例锁到 16:9", c.cb_aspect.currentText() == "16:9")
    a, b = c.kb_a, c.kb_b
    check("起点铺满整幅", a == (0, 0, 960, 540), str(a))
    check("终点往里收", b[0] > 0 and b[2] < a[2] and b[3] < a[3]
          and abs(b[2] / b[3] - 16 / 9) < 0.02, str(b))
    check("画面画出另一个框", c.view.kb_alt == b and c.view.kb_alt_name == "终点")
    check("默认就在推近", "推进" in c.lb_kb.text(), c.lb_kb.text())

    # 换比例：两个框一起重铺，运镜幅度不能被夹没了
    c.cb_aspect.setCurrentText("4:3")
    pump(app, 120)
    check("换比例后两框同比例",
          abs(c.kb_a[2] / c.kb_a[3] - 4 / 3) < 0.02 and
          abs(c.kb_b[2] / c.kb_b[3] - 4 / 3) < 0.02,
          f"{c.kb_a} / {c.kb_b}")
    check("换比例后终点仍在起点里",
          c.kb_b[2] < c.kb_a[2] and c.kb_b[0] >= c.kb_a[0]
          and c.kb_b[1] >= c.kb_a[1], f"{c.kb_a} / {c.kb_b}")
    c.cb_aspect.setCurrentText("16:9")
    pump(app, 120)
    check("换回 16:9 运镜还在", c.kb_a != c.kb_b and "推进" in c.lb_kb.text(),
          f"{c.kb_a} / {c.kb_b} · {c.lb_kb.text()}")

    # 画布上点虚线框就切过去改它
    c.view.kbSwitchRequested.emit()
    pump(app, 60)
    check("点另一端能切换编辑对象", c.kb_edit == "b" and c.view.sel == c.kb_b,
          f"{c.kb_edit} {c.view.sel}")
    c.view.kbSwitchRequested.emit()
    pump(app, 60)
    check("再点切回起点", c.kb_edit == "a" and c.view.sel == c.kb_a)

    # 时长/帧率改一下，下面的数字与帧数都要跟着变
    _kb_on(c)
    # 手工钉死两个框，后面每一句断言都能算出确切数字
    c.kb_a, c.kb_b = (0, 0, 960, 540), (96, 54, 768, 432)
    c._kb_show_endpoints()
    pump(app, 60)
    n = c._kb_frames()
    check("图片按秒算帧数", n == 6, f"{n}")
    check("结果数字写在提示里", f"{n} 帧" in c.lb_kb.text()
          and "320×180" in c.lb_kb.text(), c.lb_kb.text())

    out = os.path.join(TMP, "_kb_image.mp4")
    if os.path.exists(out):
        os.remove(out)
    exp = cp.kb_size(c.kb_b, 320)
    c.out_manual = True
    c.ed_out.setText(out)
    c.run()
    assert wait_job(app, wb), "运镜任务没在时限内结束"
    i1 = probe_video(FF, out) if os.path.exists(out) else None
    check("图片运镜出片", i1 is not None, c.log_view.toPlainText()[-200:])
    check("成片尺寸等于终点取景框比例", i1 and (i1["w"], i1["h"]) == exp,
          f"{i1['w']}x{i1['h']} 应为 {exp[0]}x{exp[1]}" if i1 else "")
    check("帧数与时长按设定走", i1 and abs(i1["dur"] - 0.6) < 0.12
          and abs(i1["fps"] - 10) < 0.01,
          f"{i1['dur']:.2f}s {i1['fps']}fps" if i1 else "")
    check("中间帧目录出片后删掉", not os.path.isdir(out[:-4] + "_运镜帧"))
    check("图片成片没有音轨", i1 and not probe_audio(FF, out),
          str(probe_audio(FF, out)) if i1 else "")
    # 取帧要落在最后一帧的显示区间里：-ss 比的是帧的 PTS，越界就一帧都取不出
    f0, f1 = frame(out, 0.0), frame(out, (n - 1) / float(c._kb_fps()) - 0.02)
    check("第一帧是整幅画面", f0 is not None and abs(f0[:, 0].mean()) < 8,
          f"{f0[:, 0].mean():.1f}" if f0 is not None else "")
    check("最后一帧左边被推掉了",
          f1 is not None and f1[:, 0].mean() > f0[:, 0].mean() + 8,
          f"{f0[:, 0].mean():.1f} -> {f1[:, 0].mean():.1f}"
          if f0 is not None and f1 is not None else "")
    check("运镜结论写在结果卡上",
          c.result.lb_title.text().startswith("运镜完成"),
          c.result.lb_title.text())
    keys = [m[0] for m in c.result._metrics]
    check("运镜指标条数齐", keys == ["成片尺寸", "帧数", "时长", "运镜幅度",
                                    "方式", "音轨", "文件体积"], str(keys))
    vals = dict((m[0], m[1]) for m in c.result._metrics)
    check("指标数字对得上", vals.get("帧数") == "6"
          and vals.get("成片尺寸") == "320×180"
          and "推进" in (vals.get("运镜幅度") or "")
          and vals.get("音轨") == "无（图片）", str(vals))
    check("成片进调色台流水线",
          wb.material.kind == "video"
          and os.path.normpath(wb.material.path) == os.path.normpath(out),
          wb.material.label())
    _shoot(wb, "运镜面板")
    return wb


def test_video_gui(app, wb):
    c = wb.module(cropper.Cropper.name)
    c.load(MOVIE)
    pump(app, 500)
    check("视频已装载", c.info and not c.is_image and c.info["w"] == 640,
          c.lb_info.text())
    check("换素材后取景框按新画面重铺",
          c.kb_a is not None and c.kb_a[2] == 640, str(c.kb_a))
    check("运镜仍然开着（记住上次）", c._kb_on())
    check("视频运镜也收掉输出模式", c.box_mode.isVisible() is False,
          c.lb_out_note.text())
    _kb_on(c, side="640", fps="10")
    pump(app, 80)
    check("重新铺好起止框", c.kb_a == (0, 0, 640, 360) and c.kb_b is not None,
          f"{c.kb_a} {c.kb_b}")
    n = c._kb_frames()
    check("视频按时长算帧数", n == max(2, int(round(c.info["dur"] * 10))), str(n))
    check("视频时长不让学生乱改", not c.sp_kb_sec.isEnabled(),
          c.lb_kb.text())
    check("提示里写明跟原片一样长", f"{n} 帧" in c.lb_kb.text(), c.lb_kb.text())

    out = os.path.join(TMP, "_kb_video.mp4")
    if os.path.exists(out):
        os.remove(out)
    c.out_manual = True
    c.ed_out.setText(out)
    c.run()
    assert wait_job(app, wb), "运镜任务没在时限内结束"
    i1 = probe_video(FF, out) if os.path.exists(out) else None
    check("视频运镜出片", i1 is not None, c.log_view.toPlainText()[-200:])
    check("成片跟着终点框尺寸", i1 and (i1["w"], i1["h"]) == (640, 360),
          f"{i1['w']}x{i1['h']}" if i1 else "")
    check("运镜不改这条片子有多长",
          i1 and abs(i1["dur"] - c.info["dur"]) < 0.15,
          f"{i1['dur']:.2f}s vs {c.info['dur']:.2f}s" if i1 else "")
    a1 = probe_audio(FF, out) if os.path.exists(out) else None
    check("逐帧重画之后音轨还在", a1 is not None and a1.get("codec") == "aac",
          str(a1))
    check("视频运镜的帧目录也删干净", not os.path.isdir(out[:-4] + "_运镜帧"))

    # ---- 取消：渲帧阶段被砍，成片与中间帧都不能留下 ----
    part = os.path.join(TMP, "_kb_cancel.mp4")
    if os.path.exists(part):
        os.remove(part)
    c.out_manual = True
    c.ed_out.setText(part)
    _kb_on(c, side="1280 (720p)", fps="24", sec=6.0)
    c.run()
    pump(app, 400)
    running = wb._job is not None and wb._job.isRunning()
    fd = part[:-4] + "_运镜帧"
    check("取消时确实渲到一半", running and (os.path.isdir(fd)
                                            or os.path.exists(part)))
    wb.cancel_job()
    assert wait_job(app, wb), "取消后任务没停下来"
    pump(app, 400)
    check("取消后不留半成品成片", not os.path.exists(part),
          c.log_view.toPlainText()[-160:].replace("\n", " "))
    check("取消后不留中间帧", not os.path.isdir(fd), fd)

    # ---- 起点=终点：镜头不会动，直接拦住 ----
    warns = _warns()
    _kb_on(c, side="320", fps="10")
    c.kb_b = c.kb_a
    c.run()
    pump(app, 120)
    check("起点等于终点会被拦下", any("起点和终点" in w for w in warns),
          str(warns[-1] if warns else ""))
    check("拦下时没有起任务", wb._job is None or not wb._job.isRunning())

    # ---- 关掉运镜：参数组收起来，输出模式长回来 ----
    c.ck_kb.setChecked(False)
    pump(app, 120)
    check("取消勾选后收起运镜参数", not c.kb_body.isVisible())
    check("输出模式回来了", c.box_mode.isVisible())
    check("自由比例重新可用", c.cb_aspect.model().item(0).isEnabled())
    check("按钮改回裁剪", c.btn_run.text() == "开始裁剪", c.btn_run.text())
    check("画面上的虚线框撤掉", c.view.kb_alt is None)
    return c


def test_persist(app):
    """重新开一次工作台，运镜的档位要还在（用户调好一次不该再调第二次）。"""
    wb = _setup(app, [cropper.Cropper])
    c = wb.module(cropper.Cropper.name)
    check("运镜开关记住上次", c._kb_on())
    check("长边记住上次", c.cb_kb_side.currentText() == "1280 (720p)",
          c.cb_kb_side.currentText())
    check("帧率记住上次", c.cb_kb_fps.currentText() == "24",
          c.cb_kb_fps.currentText())
    check("编码记住上次", c.cb_kb_codec.currentText().startswith("H.264"),
          c.cb_kb_codec.currentText())
    check("缓动记住上次", c.cb_kb_ease.currentData() == "smooth",
          str(c.cb_kb_ease.currentData()))
    c.load(STILL)
    pump(app, 400)
    check("图片进来时长又能改了", c.sp_kb_sec.isEnabled(), c.lb_kb.text())
    check("旧取景框按新画面重铺", c.kb_a == (0, 0, 960, 540)
          and c.kb_b is not None, f"{c.kb_a} {c.kb_b}")
    wb.close()
    pump(app, 80)


def _shoot(wb, name):
    p = os.path.join(SHOTS, f"shot_{name}.png")
    wb.grab().save(p)
    return p, os.path.getsize(p)


def main():
    if FF is None or not make_src():
        print("SKIP  没有可用的 ffmpeg，跳过运镜测试")
        return
    test_geometry()
    test_cmd()
    # 运镜页会读上次的开关与档位；测试落到临时 INI，不碰用户偏好
    ini = os.path.join(TMP, "settings")
    shutil.rmtree(ini, ignore_errors=True)
    QtCore.QSettings.setDefaultFormat(QtCore.QSettings.Format.IniFormat)
    QtCore.QSettings.setPath(QtCore.QSettings.Format.IniFormat,
                             QtCore.QSettings.Scope.UserScope, ini)
    app = QtWidgets.QApplication(sys.argv)
    theme.apply(app)
    wb = test_image_gui(app)
    test_video_gui(app, wb)
    test_persist(app)
    print("\n全部通过" if not FAILS else f"\n失败 {len(FAILS)} 项：{'、'.join(FAILS)}")
    sys.exit(1 if FAILS else 0)


if __name__ == "__main__":
    main()
