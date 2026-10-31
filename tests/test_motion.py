# -*- coding: utf-8 -*-
"""重定时回归：映射/取样/变调的数学，加视频与图片序列两条端到端出片。

算法全在 `acceptance/reference/motion.py`（界面只管取值和渲染），所以先把数学钉死：
匀速 1× 必须落在整数帧号上、倍速与帧率怎么算帧数、邻帧混合的权重、
运动模糊的窗口随速度变宽、atempo 串链乘回来必须等于倍速。
界面这边钉"参数真的进到成片里"：成片帧数/时长/尺寸/音轨、曲线变速时声音
只能保持原速、取消不留中间帧目录、参数记得住。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_motion.py
"""
import os
import shutil
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "acceptance"))
TMP = os.path.join(HERE, "_tmp_motion")
SHOTS = os.path.join(ROOT, "shots")
os.makedirs(TMP, exist_ok=True)

import cv2                                      # noqa: E402
import numpy as np                              # noqa: E402
from PySide6 import QtCore, QtWidgets           # noqa: E402

from reference import motion as mo                  # noqa: E402
from reference import video as vmod                 # noqa: E402
from reference.ff import (find_ffmpeg, probe_audio, probe_video,   # noqa: E402
                      run as ff_run)
from reference.gui import retime as rtmod, theme, workbench        # noqa: E402

FF = find_ffmpeg()
MOVIE = os.path.join(TMP, "_rt_src.mp4")
SRCDIR = os.path.join(TMP, "_rt_series")
FAILS = []

# 序列用纯色：第 i 张整体灰度 40*i，混出来的中间帧灰度可以直接算
STEP = 40
N_SRC = 6


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


def nth_frame(video, n, name="_rt_f.png"):
    """取成片第 n 帧（按帧号选，不比 PTS，免得取到相邻帧）。"""
    out = os.path.join(TMP, name)
    if os.path.exists(out):
        os.remove(out)
    ff_run([FF, "-y", "-hide_banner", "-loglevel", "error", "-i", video,
            "-vf", f"select=eq(n\\,{n})", "-frames:v", "1", out])
    return cv2.imread(out) if os.path.exists(out) else None


def gray(img):
    return float(img.mean()) if img is not None else -1.0


# ---------------------------------------------------------------- 数学
def test_shape():
    s = mo.speed_shape(1.0, 1.0, 1.0)
    check("匀速轮廓处处是 1", mo.is_flat(s) and abs(s.mean() - 1.0) < 1e-9)
    check("没轮廓就是恒定倍速", mo.is_flat(None))
    a = mo.speed_shape(0.5, 2.0, 0.5)
    check("曲线两端落在起止点上", abs(a[0] - 0.5) < 1e-9 and abs(a[-1] - 0.5) < 1e-9,
          f"{a[0]:.2f} / {a[-1]:.2f}")
    check("中点是峰值", int(np.argmax(a)) in (len(a) // 2 - 1, len(a) // 2),
          str(int(np.argmax(a))))
    check("平滑过渡不是折线", not mo.is_flat(a)
          and abs(a[8] - a[7]) < 0.2 and abs(a[8] - a[7]) > 1e-6)
    up = mo.speed_shape(0.6, 1.0, 1.7)
    check("逐渐加快一路单调往上", bool(np.all(np.diff(up) > -1e-12)))
    check("倍速为 0 要报错", _raises(ValueError, mo.speed_shape, 0.0, 1.0, 1.0))


def test_retime():
    pos, n, total = mo.retime(30, 25.0, 25.0)
    check("匀速 1× 且帧率不变就是原样", n == 30 and abs(total - 1.2) < 1e-9
          and np.allclose(pos, np.arange(30)),
          f"{n} 帧 {total:.3f}s  pos[:4]={pos[:4]}")
    pos2, n2, t2 = mo.retime(30, 25.0, 10.0, rate=2.0)
    check("2× 快放只剩六帧", n2 == 6 and abs(t2 - 0.6) < 1e-9,
          f"{n2} 帧 {t2:.3f}s")
    check("2× 时取样号每五帧一跳", np.allclose(pos2, [0, 5, 10, 15, 20, 25]),
          str(list(pos2)))
    pos3, n3, t3 = mo.retime(30, 25.0, 10.0, rate=0.5)
    check("0.5× 慢放帧数翻倍", n3 == 24 and abs(t3 - 2.4) < 1e-9,
          f"{n3} 帧 {t3:.3f}s")
    check("慢放落在两帧之间", np.allclose(pos3[:4], [0, 1.25, 2.5, 3.75]),
          str(list(pos3[:4])))
    check("位置夹在源片内", pos3.max() <= 29.0 and pos3.min() >= 0.0)
    curve = mo.speed_shape(0.5, 2.0, 0.5)
    pc, nc, tc = mo.retime(60, 25.0, 24.0, curve, 1.0)
    check("曲线也是单调往前", bool(np.all(np.diff(pc) >= 0.0)), f"{nc} 帧")
    up_c = mo.speed_shape(0.6, 1.0, 1.7)
    pu, _nu, _tu = mo.retime(60, 25.0, 24.0, up_c, 1.0)
    gaps = np.diff(pu)
    check("越往后走得越快", gaps[:10].mean() < gaps[-10:].mean(),
          f"前 {gaps[:10].mean():.3f} 后 {gaps[-10:].mean():.3f}")
    d_first, d_mid, d_last = (np.diff(pc)[:8].mean(),
                              np.diff(pc)[24:32].mean(),
                              np.diff(pc)[-8:].mean())
    check("慢-快-慢：中间一步跨得最多", d_mid > d_first and d_mid > d_last,
          f"起 {d_first:.3f} 中 {d_mid:.3f} 止 {d_last:.3f}")
    check("两帧以下的素材不做重定时",
          _raises(ValueError, mo.retime, 1, 25.0, 24.0))
    check("帧率为 0 要报错", _raises(ValueError, mo.retime, 30, 0, 24.0))
    check("平均倍速等于源长除以片长",
          abs(mo.avg_speed(30, 25.0, 0.6) - 2.0) < 1e-9)


def test_taps():
    hi = 9
    check("不混合只取最近那帧",
          mo.taps(3.4, 0.0, mo.OFF, src_n=hi + 1) == [(3, 1.0)]
          and mo.taps(3.6, 0.0, mo.OFF, src_n=hi + 1) == [(4, 1.0)])
    t = mo.taps(3.25, 0.0, mo.MIX, src_n=hi + 1)
    check("邻帧混合按位置加权", len(t) == 2 and t[0] == (3, 0.75)
          and abs(t[1][1] - 0.25) < 1e-12, str(t))
    check("整数位置不用混", mo.taps(4.0, 0.0, mo.MIX, src_n=hi + 1) == [(4, 1.0)])
    check("权重和总是 1", all(abs(sum(w for _i, w in
                                    mo.taps(p, d, m, 5, 0.7, hi + 1)) - 1.0) < 1e-9
                              for m in (mo.OFF, mo.MIX, mo.BLUR)
                              for p, d in ((0.0, 0.5), (3.3, 1.2), (9.0, 2.0))))
    b1 = mo.taps(4.0, 0.5, mo.BLUR, 5, 1.0, hi + 1)
    b2 = mo.taps(4.0, 2.0, mo.BLUR, 5, 1.0, hi + 1)
    check("窗口越宽摊得越开", b2[-1][0] - b2[0][0] > b1[-1][0] - b1[0][0],
          f"{b1[0][0]}..{b1[-1][0]} vs {b2[0][0]}..{b2[-1][0]}")
    check("强度 0 等于只取清晰帧", mo.taps(4.0, 2.0, mo.BLUR, 5, 0.0, hi + 1)
          == mo.taps(4.0, 2.0, mo.MIX, 5, 0.0, hi + 1))
    check("取样号不会越界", all(0 <= i <= hi for m in (mo.MIX, mo.BLUR)
                                for i, _w in mo.taps(9.9, 3.0, m, 9, 1.0, hi + 1)))
    pos, _n, _t = mo.retime(N_SRC, 25.0, 10.0, rate=0.5)
    rows = mo.plan_taps(pos, N_SRC, mo.BLUR, 5, 0.7)
    check("取样表一行对一帧", len(rows) == len(pos))
    check("每行权重各自归一", all(abs(sum(w for _i, w in r) - 1.0) < 1e-9
                                  for r in rows))
    check("每行按源帧号升序", all([i for i, _w in r] == sorted(i for i, _w in r)
                                  for r in rows))
    check("半宽随位置差算出", np.allclose(mo.half_spans(np.arange(10.0)), 0.5))


def test_atempo():
    check("1× 不用改变声", mo.atempo(1.0) == "")
    check("两倍就是一级", mo.atempo(2.0) == "atempo=2", mo.atempo(2.0))
    check("四倍要串两级", mo.atempo(4.0) == "atempo=2,atempo=2", mo.atempo(4.0))
    for rate in (0.1, 0.35, 0.5, 0.8, 1.5, 3.0, 7.0, 16.0, 20.0):
        spec = mo.atempo(rate)
        back = 1.0
        for part in spec.split(","):
            back *= float(part.split("=")[1])
        check(f"atempo 串回来等于 {rate:g}×", abs(back / rate - 1.0) < 1e-5, spec)
    check("每级都在 0.5~2 内", all(0.5 - 1e-9 <= float(p.split("=")[1]) <= 2 + 1e-9
                                   for p in mo.atempo(20.0).split(",")))


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
        "ff", "frames", "out.mp4", 30, None, 320, 18, "medium", "hevc",
        "mt%05d.jpg", 0, 8, 320, 180, 0, False, progress=True,
        audio_src="src.mp4", audio_filter="atempo=2,atempo=1.5")
    s = " ".join(cmd)
    check("变速编码读的是重画出来的帧", "-i frames" + os.sep + "mt%05d.jpg" in s,
          s[:120])
    check("要变调时重编音轨而不是复制",
          "-filter:a atempo=2,atempo=1.5 -c:a aac -b:a 192k" in s, s)
    check("变速时以画面长度截断", "-shortest" in s, s[-120:])
    check("尺寸按长边给足且取偶", (w, h) == (320, 180) and f"scale={w}:{h}" in s)
    cmd2, _w, _h = vmod.build_cmd(
        "ff", "f", "o.mp4", 24, None, 320, 18, "medium", "h264",
        "mt%05d.jpg", 0, 5, 320, 180, 0, False, progress=True,
        audio_src="src.mp4")
    s2 = " ".join(cmd2)
    check("保持原速时仍然直接复制音轨",
          "-i src.mp4 -map 0:v:0 -map 1:a? -c:a copy" in s2, s2)
    check("复制时不加 -shortest（免得削掉最后一帧）", "-shortest" not in s2, s2)
    cmd3, _w, _h = vmod.build_cmd(
        "ff", "f", "o.mp4", 24, None, 320, 18, "medium", "h264",
        "mt%05d.jpg", 0, 5, 320, 180, 0, False, progress=True)
    check("序列素材没有音轨就不接第二路输入", "1:a?" not in " ".join(cmd3))


def test_out_size():
    check("横幅按长边给", rtmod.out_size(640, 360, 320) == (320, 180),
          str(rtmod.out_size(640, 360, 320)))
    check("竖幅时长边落在高上", rtmod.out_size(1080, 1920, 1080) == (608, 1080),
          str(rtmod.out_size(1080, 1920, 1080)))
    check("另一边总是偶数", all(v % 2 == 0 for v in rtmod.out_size(640, 420, 3840)))
    check("标签里取前导数字", rtmod.num("3840 (UHD 4K)", 0) == 3840
          and rtmod.num("29.97", 0) == 29.97 and rtmod.num("没数字", 7) == 7)


# ---------------------------------------------------------------- 素材
def make_src():
    if FF is None:
        return False
    r = ff_run([FF, "-y", "-hide_banner", "-loglevel", "error",
                "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25:duration=1.2",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=1.2",
                "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                "-c:a", "aac", "-shortest", MOVIE])
    if r.returncode != 0 or not os.path.exists(MOVIE):
        return False
    shutil.rmtree(SRCDIR, ignore_errors=True)
    os.makedirs(SRCDIR)
    for i in range(N_SRC):
        img = np.full((180, 320, 3), STEP * i, np.uint8)
        cv2.imwrite(os.path.join(SRCDIR, f"clip{i:05d}.png"), img)
    return True


def _setup(app):
    wb = workbench.Workbench([rtmod.Retime])
    wb.resize(1480, 880)
    wb.show()
    pump(app, 200)
    wb.set_module(rtmod.Retime.name)
    pump(app, 120)
    return wb, wb.module(rtmod.Retime.name)


def _warns():
    """QMessageBox.warning 是模态的，测试里换成记录文本，否则 pump 卡死。"""
    got = []
    QtWidgets.QMessageBox.warning = staticmethod(
        lambda *a, **k: got.append(a[2] if len(a) > 2 else a[-1]))
    return got


def _out(rt, path):
    """把输出钉到一个确定路径上（跳过 unique_path 的改名）。"""
    rt.out_manual = True
    rt.ed_out.setText(path)


# ---------------------------------------------------------------- 视频界面
def test_video_gui(app):
    wb, rt = _setup(app)
    warns = _warns()

    rt.load(MOVIE)
    pump(app, 600)
    check("视频已装载", rt.src and rt.src["kind"] == "video" and rt.src["w"] == 640,
          rt.lb_info.text())
    check("源帧率跟着探测走", rt._src_fps() == 25.0, str(rt._src_fps()))
    check("成片帧率默认跟上源帧率", rt._out_fps() == 25.0, str(rt._out_fps()))
    check("自带音轨被认出来", rt.src["audio"] and "音轨" in rt.lb_info.text(),
          rt.lb_info.text())
    check("顶栏报规格与时长", rt.top_hint().startswith("640×360"), rt.top_hint())
    check("预览画的是中间那一帧", rt.preview.has_image())
    check("1× 且不换帧率时提醒等于白重编",
          "原样重编码" in rt.lb_plan.text(), rt.lb_plan.text())

    # 匀速 2× 快放、不混合、320 长边、10fps
    rt.cb_mode.setCurrentIndex(rt.cb_mode.findData(mo.OFF))
    rt.sp_rate.setValue(2.0)
    rt.cb_fps.setCurrentText("10")
    rt.cb_side.setCurrentText("320")
    rt.cb_enc.setCurrentIndex(rt.cb_enc.findData("ultrafast"))
    pump(app, 120)
    check("映射算出六帧零点六秒", "成片 6 帧" in rt.lb_plan.text()
          and "0.6s" in rt.lb_plan.text(), rt.lb_plan.text())
    check("不混合时提示会看到停顿", "邻帧混合" in rt.lb_mix.text(), rt.lb_mix.text())
    check("取样数与强度只在运动模糊下可用",
          not rt.cb_taps.isEnabled() and not rt.sl_strength.isEnabled())

    out = os.path.join(TMP, "_rt_fast.mp4")
    if os.path.exists(out):
        os.remove(out)
    _out(rt, out)
    rt.run()
    assert wait_job(app, wb), "快放任务没在时限内结束"
    i1 = probe_video(FF, out) if os.path.exists(out) else None
    check("快放出片", i1 is not None, rt.log_view.toPlainText()[-200:])
    check("成片尺寸等于长边换算", i1 and (i1["w"], i1["h"]) == (320, 180),
          f"{i1['w']}x{i1['h']}" if i1 else "")
    check("2× 之后只剩一半时长", i1 and abs(i1["dur"] - 0.6) < 0.12,
          f"{i1['dur']:.2f}s" if i1 else "")
    a1 = probe_audio(FF, out) if os.path.exists(out) else None
    check("声音跟着倍速变调", a1 is not None and a1["codec"] == "aac"
          and abs(a1["dur"] - 0.6) < 0.25, str(a1))
    check("中间帧目录出片后删掉", not os.path.isdir(out[:-4] + "_变速帧"))
    check("取样是每五帧一跳（画面跳着走）",
          gray(nth_frame(out, 1)) > 0, "")
    check("结果卡结论先行", rt.result.lb_title.text().startswith("重定时完成"),
          rt.result.lb_title.text())
    st = rt.result.steps()
    check("结果卡按真实步骤走完",
          [s[0] for s in st] == ["按速度曲线混帧", "编码成片"]
          and all(s[1] == "ok" for s in st)
          and "帧" in st[0][2] and "MB" in st[1][2], str(st))
    vals = dict((m[0], m[1]) for m in rt.result.metric_items())
    keys = [m[0] for m in rt.result.metric_items()]
    check("结果卡指标齐", keys == ["成片帧数", "成片时长", "平均倍速", "源→成片",
                                  "画面规格", "画面混合", "音轨", "文件体积"],
          str(keys))
    check("指标数字对得上", vals.get("成片帧数") == "6"
          and vals.get("源→成片") == "30 → 6" and "320×180" in vals.get("画面规格")
          and "跟着倍速" in vals.get("音轨"), str(vals))
    check("成片进流水线", wb.material.kind == "video"
          and os.path.normpath(wb.material.path) == os.path.normpath(out),
          wb.material.label())
    check("指标每个都带解释",
          all(m[2] for m in rt.result.metric_items()))

    # ---- 变速曲线：声音跟不上，必须说清楚 ----
    rt.ck_curve.setChecked(True)
    pump(app, 80)
    check("勾上曲线后轮廓图长出来", rt.graph.isVisibleTo(rt.seg_row))
    check("曲线开着时起中止能改", all(rt.seg[k].isEnabled() for k in "amb"))
    rt.cb_shape.setCurrentText("慢-快-慢")
    pump(app, 80)
    check("预设把三个端点填进去",
          [rt.seg[k].value() for k in "amb"] == [0.5, 2.0, 0.5],
          str([rt.seg[k].value() for k in "amb"]))
    src, filt = rt._audio_args()
    check("曲线变速时音轨不再假装跟速", src == MOVIE and filt is None, str(filt))
    rt.sp_rate.setValue(1.0)
    pump(app, 80)
    check("轮廓按倍速画", rt.graph.values is not None and len(rt.graph.values)
          == mo.SHAPE_BINS)
    curved = os.path.join(TMP, "_rt_curve.mp4")
    if os.path.exists(curved):
        os.remove(curved)
    _out(rt, curved)
    rt.run()
    assert wait_job(app, wb), "曲线任务没在时限内结束"
    i2 = probe_video(FF, curved) if os.path.exists(curved) else None
    check("曲线也能出片", i2 is not None, rt.log_view.toPlainText()[-160:])
    check("曲线成片仍然带原声",
          i2 and probe_audio(FF, curved) is not None
          and abs(probe_audio(FF, curved)["dur"] - 1.2) < 0.2,
          str(probe_audio(FF, curved)) if i2 else "")
    check("声画会错位写在结果卡上", rt.result.status_kind() == "warn"
          and "对不上" in (rt.result.btn_mark.toolTip() or ""),
          f"{rt.result.status_kind()} {rt.result.btn_mark.toolTip()}")
    vals2 = dict((m[0], m[1]) for m in rt.result.metric_items())
    check("指标里说明声音保持原速", "原速" in (vals2.get("音轨") or ""),
          str(vals2.get("音轨")))

    # ---- 运动模糊：窗口参数这时候才解锁 ----
    rt.cb_mode.setCurrentIndex(rt.cb_mode.findData(mo.BLUR))
    pump(app, 80)
    check("运动模糊下取样数与强度可用",
          rt.cb_taps.isEnabled() and rt.sl_strength.isEnabled())
    check("模糊强度显示百分比", rt.lb_strength.text() == "70%", rt.lb_strength.text())
    check("提示报平均每张混几帧", "混" in rt.lb_mix.text(), rt.lb_mix.text())
    blurred = os.path.join(TMP, "_rt_blur.mp4")
    if os.path.exists(blurred):
        os.remove(blurred)
    rt.ck_curve.setChecked(False)
    _out(rt, blurred)
    rt.run()
    assert wait_job(app, wb), "模糊任务没在时限内结束"
    check("运动模糊出片", os.path.exists(blurred),
          rt.log_view.toPlainText()[-160:])

    # ---- 拦一下没意义的请求 ----
    warns.clear()
    rt.src = None
    rt.run()
    pump(app, 120)
    check("没素材时拦住并说明", any("先选择" in w for w in warns), str(warns))
    return wb, rt


# ---------------------------------------------------------------- 序列界面
def test_series_gui(app):
    wb, rt = _setup(app)
    rt.load(SRCDIR)
    pump(app, 500)
    check("序列已装载", rt.src and rt.src["kind"] == "series"
          and rt.src["n"] == N_SRC, rt.lb_info.text())
    check("序列没有帧率，靠源帧率旋钮声明", rt._src_fps() > 0, str(rt._src_fps()))
    rt.sp_src_fps.setValue(25.0)
    rt.sp_rate.setValue(0.5)
    rt.cb_fps.setCurrentText("10")
    rt.cb_side.setCurrentText("320")
    rt.cb_mode.setCurrentIndex(rt.cb_mode.findData(mo.MIX))
    rt.cb_enc.setCurrentIndex(rt.cb_enc.findData("ultrafast"))
    pump(app, 120)
    # 0.5× 慢放：pos = 1.25*i，第 1 帧是源第 1、2 张的 3/4 + 1/4
    want = STEP * 1.25
    check("混出来的灰度算得出来", abs(want - 50) < 1e-6, str(want))
    out = os.path.join(TMP, "_rt_slow.mp4")
    if os.path.exists(out):
        os.remove(out)
    _out(rt, out)
    rt.run()
    assert wait_job(app, wb), "慢放任务没在时限内结束"
    i1 = probe_video(FF, out) if os.path.exists(out) else None
    check("序列慢放出片", i1 is not None, rt.log_view.toPlainText()[-200:])
    check("成片帧数按 0.5× 算出来", i1 and abs(i1["dur"] - 0.48) < 0.12,
          f"{i1['dur']:.2f}s" if i1 else "")
    g = gray(nth_frame(out, 1))
    check("邻帧混合真的把相邻两帧叠起来了", abs(g - want) <= 3.0,
          f"第 2 帧灰度 {g:.1f}，混出来应是 {want:.1f}，"
          f"单帧是 {STEP} 或 {STEP * 2}")
    check("首帧还是源第一帧", abs(gray(nth_frame(out, 0)) - 0) <= 3,
          f"{gray(nth_frame(out, 0)):.1f}")

    # 不混合：同一位置该取整帧
    rt.cb_mode.setCurrentIndex(rt.cb_mode.findData(mo.OFF))
    pump(app, 80)
    out2 = os.path.join(TMP, "_rt_slow_off.mp4")
    if os.path.exists(out2):
        os.remove(out2)
    _out(rt, out2)
    rt.run()
    assert wait_job(app, wb), "不混合任务没在时限内结束"
    g2 = gray(nth_frame(out2, 1))
    check("不混合时同一帧保持干净", abs(g2 - STEP) <= 3.0,
          f"第 2 帧灰度 {g2:.1f}，应等于第 1 张源帧 {STEP}")
    check("不混合时结果卡提醒会停顿", rt.result.status_kind() == "warn"
          and "混合" in (rt.result.btn_mark.toolTip() or ""),
          f"{rt.result.status_kind()} {rt.result.btn_mark.toolTip()}")

    # ---- 运动模糊：把快门窗口里的几张源帧平均掉，比邻帧混合摊得更开 ----
    rt.cb_mode.setCurrentIndex(rt.cb_mode.findData(mo.BLUR))
    rt.sl_strength.setValue(100)
    rt.cb_taps.setCurrentText("5")
    pump(app, 80)
    # pos=1.25、半宽 0.625 → 窗口取到源第 1、2 张，权重 0.6 / 0.4
    want_b = STEP * (0.6 * 1 + 0.4 * 2)
    blur = os.path.join(TMP, "_rt_blur_series.mp4")
    if os.path.exists(blur):
        os.remove(blur)
    _out(rt, blur)
    rt.run()
    assert wait_job(app, wb), "模糊任务没在时限内结束"
    gb = gray(nth_frame(blur, 1))
    check("运动模糊按窗口摊开", abs(gb - want_b) <= 3.0,
          f"第 2 帧灰度 {gb:.1f}，窗口平均应是 {want_b:.1f}（邻帧混合 {STEP * 1.25:.0f}、"
          f"单帧 {STEP}）")
    check("模糊强度满档写在结果卡上",
          "运动模糊" in dict((m[0], m[1]) for m in rt.result.metric_items())
          .get("画面混合", ""),
          str(rt.result.metric_items()))

    # ---- 取消：混帧阶段砍掉，成片与中间帧都不能留 ----
    part = os.path.join(TMP, "_rt_cancel.mp4")
    for p in (part, part[:-4] + "_变速帧"):
        if os.path.exists(p):
            (shutil.rmtree(p) if os.path.isdir(p) else os.remove(p))
    rt.sp_rate.setValue(0.1)
    rt.cb_fps.setCurrentText("60")
    rt.cb_side.setCurrentText("3840 (UHD 4K)")
    rt.cb_mode.setCurrentIndex(rt.cb_mode.findData(mo.BLUR))
    rt.out_manual = True
    rt.ed_out.setText(part)
    rt.run()
    pump(app, 500)
    fd = part[:-4] + "_变速帧"
    running = wb._job is not None and wb._job.isRunning()
    check("取消时确实渲到一半", running and os.path.isdir(fd),
          f"running={running} 帧目录={os.path.isdir(fd)}")
    wb.cancel_job()
    assert wait_job(app, wb), "取消后任务没停下来"
    pump(app, 400)
    check("取消后不留半成品成片", not os.path.exists(part),
          rt.log_view.toPlainText()[-160:].replace("\n", " "))
    check("取消后不留中间帧", not os.path.isdir(fd), fd)
    check("取消写在结果卡上", rt.result.lb_title.text().startswith("已取消"),
          rt.result.lb_title.text())

    # ---- 记住参数：重开一次还在（曲线、混合、长边都别丢）----
    rt._save()
    wb.close()
    pump(app, 120)
    wb2, rt2 = _setup(app)
    check("倍速记住上次", rt2.sp_rate.value() == 0.1, str(rt2.sp_rate.value()))
    check("混合模式记住上次", rt2._mode() == mo.BLUR, str(rt2._mode()))
    check("源帧率记住上次", rt2._src_fps() == 25.0, str(rt2._src_fps()))
    check("长边跟源记住上次", rt2.ck_follow.isChecked() == False
          and rt2.cb_side.currentText().startswith("3840"),
          rt2.cb_side.currentText())
    check("编码速度记住的是档位不是标签", rt2._preset() == "ultrafast",
          str(rt2._preset()))
    check("六个工具的讲解都挂上了",
          rt2.lb_info.style() is not None and rt2.ed_src.toolTip().startswith("【"),
          rt2.ed_src.toolTip()[:20])
    wb2.set_module(rtmod.Retime.name)
    pump(app, 150)
    p = os.path.join(SHOTS, "shot_运动面板.png")
    wb2.grab().save(p)
    check("运动页截图", os.path.getsize(p) > 10000,
          f"{os.path.basename(p)} {os.path.getsize(p) // 1024}KB")
    wb2.close()
    pump(app, 120)


def main():
    test_shape()
    test_retime()
    test_taps()
    test_atempo()
    test_cmd()
    test_out_size()
    if FF is None or not make_src():
        print("SKIP  没有可用的 ffmpeg，跳过重定时端到端测试")
        print(f"\n失败 {len(FAILS)} 项：{'、'.join(FAILS)}" if FAILS else "\n数学部分全部通过")
        sys.exit(1 if FAILS else 0)
    ini = os.path.join(TMP, "settings")
    shutil.rmtree(ini, ignore_errors=True)
    QtCore.QSettings.setDefaultFormat(QtCore.QSettings.Format.IniFormat)
    QtCore.QSettings.setPath(QtCore.QSettings.Format.IniFormat,
                             QtCore.QSettings.Scope.UserScope, ini)
    app = QtWidgets.QApplication.instance() or QtWidgets.QApplication(sys.argv)
    theme.apply(app)
    test_video_gui(app)
    test_series_gui(app)
    print("\n全部通过" if not FAILS else f"\n失败 {len(FAILS)} 项：{'、'.join(FAILS)}")
    sys.exit(1 if FAILS else 0)


if __name__ == "__main__":
    main()
