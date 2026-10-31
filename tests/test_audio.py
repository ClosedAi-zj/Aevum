# -*- coding: utf-8 -*-
"""配乐：命令层单测 + 真混流端到端（延迟/混音/循环都解码回波形去量）。

长度算式与命令都在 acceptance/reference/audio.py，先钉死纯函数；再走真界面：
装视频 -> 装配乐 -> 出片 -> 用 ffmpeg 把成片的音频解成 wav，数静音、做 FFT。
"文件里有一条音频流"证明不了延迟真的生效，只有波形上的静音长度能证明。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_audio.py
"""
import array
import os
import shutil
import sys
import time
import wave

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "acceptance"))
TMP = os.path.join(HERE, "_tmp")
SHOTS = os.path.join(ROOT, "shots")
os.makedirs(TMP, exist_ok=True)

import numpy as np                            # noqa: E402
from PySide6 import QtCore, QtWidgets         # noqa: E402

from reference import audio as au                 # noqa: E402
from reference.ff import (find_ffmpeg, probe_audio, probe_video,   # noqa: E402
                      run as ff_run)
from reference.gui import audiomix, theme, workbench               # noqa: E402

FF = find_ffmpeg()
SRC = os.path.join(TMP, "_mix_src.mp4")       # 4s 画面 + 440Hz 原声
MUS = os.path.join(TMP, "_mix_mus.mp3")       # 1.5s 880Hz：比画面短，好测循环
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


def opt(**kw):
    return {**au.DEFAULTS, **kw}


# 用 lavfi sine 造的单音解回波形峰值实测约 0.12，汉宁窗 FFT 读回来约 0.06。
# 阈值按这个电平定，比"文件里有一条音频流"能分辨出音量真的生效了。
TONE = 0.02          # 这条频率确实响了
LOUD = 0.04          # 满音量的一路（没被衰减）
QUIET = 0.005        # 这条频率不该存在

# 选项名 -> 界面控件：滑杆和数字框的名字不一样，别在测试里硬拼前缀
WIDGET = {"delay": "sp_delay", "gain": "sl_gain", "orig_gain": "sl_orig",
          "fade_in": "sp_fin", "fade_out": "sp_fout"}


def wait_job(app, wb, timeout=120):
    t0 = time.time()
    while (wb._job is not None and wb._job.isRunning()
            and time.time() - t0 < timeout):
        pump(app, 50)
    pump(app, 200)
    return wb._job is None or not wb._job.isRunning()


# ---------------------------------------------------------------------- 纯函数
def test_pure():
    o = opt()
    check("默认值不转码", not au.needs_filter(o) and au.copy_ok(".mp4", "aac", o))
    check("mp3 也能原样进 mp4", au.copy_ok(".mp4", "mp3", o))
    check("flac 进 mp4 得转码", not au.copy_ok(".mp4", "flac", o))
    check("mkv 什么都收", au.copy_ok(".mkv", "flac", o))
    for k in ("delay", "gain", "fade_in", "fade_out"):
        v = 10 if k == "gain" else 1
        check(f"{k} 一改就得重编", au.needs_filter(opt(**{k: v})))
    check("混合就得重编", au.needs_filter(opt(mode="mix")))
    check("循环本身不用滤镜", not au.needs_filter(opt(loop=True)))

    # 长度：画面 4s、配乐 1.5s
    check("不循环不铺满时配乐早早收", au.music_end(4, 1.5, opt()) == 1.5)
    check("循环铺到片尾", au.music_end(4, 1.5, opt(loop=True)) == 4)
    check("晚进 1 秒顶到片尾",
          au.music_end(4, 3.0, opt(delay=1.0, trim=True)) == 4.0)
    check("不截断时配乐拖长成片", au.total_dur(4, 6, opt(trim=False)) == 6)
    check("截断后成片等于画面", au.total_dur(4, 6, opt(trim=True)) == 4)
    check("循环必须给 -t 否则无限", au.limit_dur(4, 1.5, opt(loop=True)) == 4)
    check("不截断就不给 -t", au.limit_dur(4, 6, opt(trim=False)) == 0)

    # 命令里的假探测结果按 ff.probe_* 的实际字段给：codec 缺了会被当成"编码未知"
    V = {"dur": 4, "audio": True}
    A = {"dur": 1.5, "codec": "mp3", "sr": 44100, "layout": "mono"}
    c, _d = au.build_cmd("ff", "V", "A", "O.mp4", V, A, opt())
    s = " ".join(c)
    check("纯替换走流复制",
          "-c:v copy -c:a copy" in s and "-filter_complex" not in s, s)
    check("只取第一条视频流", "-map 0:v:0 -map 1:a:0" in s, s)
    check("mp4 要 faststart", "-movflags +faststart" in s)
    c2, _ = au.build_cmd("ff", "V", "A", "O.mp4", V, A,
                         opt(delay=1.0, gain=60, fade_in=0.5, fade_out=0.5))
    s2 = " ".join(c2)
    check("延迟用 adelay 不用 itsoffset", "adelay=1000:all=1" in s2
          and "-itsoffset" not in s2, s2)
    check("音量进滤镜", "volume=0.6000" in s2)
    check("淡入按延迟起算", "afade=t=in:st=1.000:d=0.500" in s2, s2)
    # 配乐 1.5s 晚进 1s = 2.5s 收尾，淡出 0.5s 从 2.0s 起
    check("淡出落在真正收尾处", "afade=t=out:st=2.000:d=0.500" in s2, s2)
    c3, _ = au.build_cmd("ff", "V", "A", "O.mp4", V, A,
                         opt(mode="mix", loop=True, acodec="aac256"))
    s3 = " ".join(c3)
    check("循环用 stream_loop", "-stream_loop -1" in s3)
    check("混音不衰减", "amix=inputs=2:duration=longest:normalize=0" in s3, s3)
    check("指定码率照办", "-c:a aac -b:a 256k" in s3)
    check("要重编就不给复制", "-c:a copy" not in s3)
    _c4, d4 = au.build_cmd("ff", "V", "A", "O.mkv", V,
                           {"dur": 9, "codec": "flac"}, opt(trim=False))
    check("进度总长按成片算", abs(d4 - 9.0) < 1e-6, str(d4))
    check("flac 进 mkv 也能原样搬", "-c:a copy" in " ".join(_c4))


# ------------------------------------------------------------------ 造素材
def make_src():
    if FF is None:
        return False
    if os.path.exists(SRC):
        os.remove(SRC)
    r = ff_run([FF, "-y", "-hide_banner", "-loglevel", "error",
                "-f", "lavfi",
                "-i", "testsrc2=size=640x360:rate=25:duration=4",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=4",
                "-c:v", "libx264", "-preset", "ultrafast",
                "-pix_fmt", "yuv420p",
                "-c:a", "aac", "-shortest", SRC])
    if r.returncode != 0 or not os.path.exists(SRC):
        return False
    if os.path.exists(MUS):
        os.remove(MUS)
    r = ff_run([FF, "-y", "-hide_banner", "-loglevel", "error",
                "-f", "lavfi", "-i", "sine=frequency=880:duration=1.5",
                "-c:a", "libmp3lame", "-b:a", "192k", MUS])
    return r.returncode == 0 and os.path.exists(MUS)


def wav_of(path, sr=8000):
    """把成片里的音频解成单声道 wav，返回采样数组。"""
    out = os.path.join(TMP, "_mix_check.wav")
    if os.path.exists(out):
        os.remove(out)
    ff_run([FF, "-y", "-hide_banner", "-loglevel", "error", "-i", path,
            "-vn", "-ac", "1", "-ar", str(sr), out])
    if not os.path.exists(out):
        return None, sr
    with wave.open(out) as w:
        data = array.array("h", w.readframes(w.getnframes()))
    return np.frombuffer(data, dtype=np.int16).astype(float) / 32768.0, sr


def tone_at(a, sr, freq):
    """给定窗口里这个频率的能量（相对满量程），用来确认声音是谁。"""
    if a is None or len(a) < sr // 2:
        return 0.0
    n = min(len(a), sr)
    mag = np.abs(np.fft.rfft(a[:n] * np.hanning(n)))
    k = int(round(freq * n / sr))
    return float(mag[max(0, k - 3):k + 4].max() * 2 / n)


def lead_silence(a, sr):
    if a is None:
        return -1.0
    hit = np.flatnonzero(np.abs(a) > 0.02)
    return float(hit[0] / sr) if len(hit) else 999.0


# -------------------------------------------------------------------- 界面
def test_gui(app):
    wb = workbench.Workbench([audiomix.AudioMix])
    wb.resize(1480, 880)
    wb.show()
    pump(app, 200)
    m = wb.module(audiomix.AudioMix.name)

    from apps.workbench import MODULES
    check("已注册为第五个工具",
          [c.name for c in MODULES]
          == ["调色台", "对齐", "合成", "裁剪", "配乐", "运动"],
          str([c.name for c in MODULES]))
    check("配乐无停靠面板",
          not [d for d in wb.findChildren(QtWidgets.QDockWidget)
               if d.isVisible()])

    m._load_video(SRC)
    pump(app, 400)
    check("视频已探测", m.vinfo is not None and m.vinfo["w"] == 640,
          m.lb_info.text())
    check("认出视频自带音轨", m.vinfo["audio"] is True, m.lb_info.text())
    check("混合这条路可用", m.rb["mix"].isEnabled())
    check("还没配乐不能算长度", m.ainfo is None and m.lb_len.text()
          .startswith("选好两段素材"))

    m._load_audio(MUS)
    pump(app, 400)
    check("配乐已探测", m.ainfo is not None and m.ainfo["codec"] == "mp3",
          m.lb_info.text())
    check("输出默认叫 _mix", m.ed_out.text().endswith("_mix.mp4"),
          m.ed_out.text())

    # 探测失败的红色必须在换掉素材后退回去：objectName 改了不重刷样式就会赖着
    junk = os.path.join(TMP, "_mix_junk.mp4")
    with open(junk, "wb") as f:
        f.write(b"not a video at all")
    m._load_video(junk)
    pump(app, 200)
    check("读不到视频流时信息条报红", m.lb_info.objectName() == "InfoBad",
          f"{m.lb_info.objectName()}｜{m.lb_info.text()}")
    m._load_video(SRC)
    pump(app, 400)
    check("换回正常视频后红色退回普通",
          m.lb_info.objectName() == "Info" and m.vinfo is not None,
          f"{m.lb_info.objectName()}｜{m.lb_info.text()}")
    check("配乐短于画面就提醒静音", "没有声音" in m.lb_len.text(), m.lb_len.text())
    # 时间线取的是探测长度：mp3 带编码器边距，1.5s 的音读出来 1.54s，别按字面相等断言
    check("时间线按两段长度画", abs(m.timeline.out - 4.0) < 0.05
          and 1.4 < m.timeline.end < 1.75,
          f"out={m.timeline.out} end={m.timeline.end}")
    # 不转码此刻是合法的（mp3 能进 mp4），勾了延迟就该灰掉
    mdl = m.cb_acodec.model()
    icopy = m.cb_acodec.findData("copy")
    check("能复制时不转码可选", mdl.item(icopy).isEnabled(), str(icopy))
    m.sp_delay.setValue(1.0)
    pump(app, 120)
    check("要重编时不转码被灰掉", not mdl.item(icopy).isEnabled())
    check("灰掉后自动退回自动档", m.cb_acodec.currentData() == "auto",
          m.cb_acodec.currentText())
    m.sp_delay.setValue(0.0)
    pump(app, 120)

    # ---- 1) 纯替换：音频原样复制，画面一个比特不改 ----
    out1 = os.path.join(TMP, "_mix_copy.mp4")
    _run(m, wb, app, out1, {})
    i1 = probe_video(FF, out1) if os.path.exists(out1) else None
    a1 = probe_audio(FF, out1) if i1 else None
    check("替换出片", i1 is not None, m.log_view.toPlainText()[-160:])
    check("原声真的被丢掉，只留配乐",
          a1 is not None and a1["codec"] == "mp3", str(a1))
    w1, sr = wav_of(out1)
    l880 = tone_at(w1, sr, 880)
    check("成片里只有 880Hz 没有 440Hz",
          l880 > TONE and tone_at(w1, sr, 440) < QUIET,
          f"880={l880:.3f} 440={tone_at(w1, sr, 440):.3f}")
    # 复制模式下音频流就按配乐自身长度收尾，画面继续走到 4s：后面那段是没声音的
    check("配乐按自身长度收尾、画面照常走完",
          abs(i1["dur"] - 4.0) < 0.3 and 1.3 < len(w1) / sr < 1.75,
          f"画面 {i1['dur']:.2f}s  音频 {len(w1) / sr:.2f}s")
    f0 = os.path.join(TMP, "_mix_f0.png")
    f1 = os.path.join(TMP, "_mix_f1.png")
    ff_run([FF, "-y", "-hide_banner", "-loglevel", "error", "-ss", "2",
            "-i", SRC, "-frames:v", "1", f0])
    ff_run([FF, "-y", "-hide_banner", "-loglevel", "error", "-ss", "2",
            "-i", out1, "-frames:v", "1", f1])
    same = _png_equal(f0, f1)
    check("画面是流复制的（同一帧逐像素相同）", same, str(same))

    # ---- 2) 延迟：静音得是真采样，容器里的偏移播放器不一定认 ----
    out2 = os.path.join(TMP, "_mix_delay.mp4")
    _run(m, wb, app, out2, {"delay": 1.0})
    w2, sr = wav_of(out2)
    lead = lead_silence(w2, sr)
    check("晚进 1 秒真的补了 1 秒静音", 0.95 <= lead <= 1.10, f"起声 {lead:.3f}s")
    check("延迟后必然转码", probe_audio(FF, out2)["codec"] == "aac",
          str(probe_audio(FF, out2)))

    # ---- 3) 混合：两路都在，且没被 amix 默认除以 2 ----
    out3 = os.path.join(TMP, "_mix_mix.mp4")
    _run(m, wb, app, out3, {"mode": "mix", "gain": 50, "orig_gain": 100})
    w3, sr = wav_of(out3)
    t880, t440 = tone_at(w3, sr, 880), tone_at(w3, sr, 440)
    check("混合后两路都在", t880 > TONE and t440 > LOUD,
          f"880={t880:.3f} 440={t440:.3f}")
    solo = tone_at(wav_of(SRC)[0], sr, 440)
    check("amix 没有把原声偷偷减半", t440 > solo * 0.75,
          f"混合后 {t440:.3f} / 原声 {solo:.3f}")
    check("配乐音量 50 真的降下来", t880 < l880 * 0.7,
          f"半音量 {t880:.3f} / 满音量 {l880:.3f}")

    # ---- 4) 循环铺满 ----
    out4 = os.path.join(TMP, "_mix_loop.mp4")
    _run(m, wb, app, out4, {"loop": True})
    w4, sr = wav_of(out4)
    tail = w4[int(3.0 * sr):int(3.4 * sr)]
    check("1.5 秒的配乐循环到 3 秒后还在响",
          tail is not None and float(np.abs(tail).max()) > 0.05,
          f"峰值 {0.0 if tail is None else np.abs(tail).max():.3f}")
    check("循环不改变成片长度", abs(len(w4) / sr - 4.0) < 0.3,
          f"{len(w4) / sr:.2f}s")

    # ---- 5) 原文件不动 + 成片进流水线 ----
    src_a = probe_audio(FF, SRC)
    check("源视频没被改", src_a["codec"] == "aac", str(src_a))
    check("配乐写不进源文件", os.path.normpath(m.ed_video.text())
          == os.path.normpath(SRC), m.ed_video.text())
    check("成片进调色台流水线", wb.material.kind == "video"
          and os.path.normpath(wb.material.path) == os.path.normpath(out4),
          wb.material.label())

    # ---- 6) 不截断：画面完了声音还在 ----
    long_mus = os.path.join(TMP, "_mix_long.m4a")
    ff_run([FF, "-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi",
            "-i", "sine=frequency=660:duration=6", "-c:a", "aac", long_mus])
    m._load_audio(long_mus)
    pump(app, 300)
    check("截断生效要说截掉了多少", "截掉" in m.lb_len.text(), m.lb_len.text())
    m.cb_trim.setChecked(False)
    pump(app, 120)
    check("不截断就提示拖尾", "还在响" in m.lb_len.text(), m.lb_len.text())
    out5 = os.path.join(TMP, "_mix_tail.mp4")
    _run(m, wb, app, out5, {})
    i5 = probe_video(FF, out5)
    check("不截断时成片按配乐长度走", i5 and 5.5 < i5["dur"] < 6.6,
          f"{i5['dur']:.2f}s" if i5 else "")

    # ---- 7) 界面画得出来 ----
    m.cb_trim.setChecked(True)
    m._load_audio(MUS)
    m.sp_fout.setValue(0.5)
    m.sl_gain.setValue(80)
    pump(app, 200)
    shot = os.path.join(SHOTS, "shot_配乐.png")
    wb.grab().save(shot)
    check("配乐界面截图", os.path.getsize(shot) > 60000,
          f"{os.path.getsize(shot) // 1024}KB")
    n = _accent_pixels(shot, m.timeline)
    check("时间线画出了配乐条", n > 400, f"{n} px")
    wb.close()
    pump(app, 200)


def _accent_pixels(shot, tl):
    """数时间线区域里有多少属于强调色的像素：画没画一眼可见，但得能量出来。"""
    from PySide6 import QtGui
    img = QtGui.QImage(shot)
    if img.isNull():
        return 0
    pos = tl.mapTo(wb_top(tl), QtCore.QPoint(0, 0))
    rgb = img.convertToFormat(QtGui.QImage.Format.Format_RGB32)
    buf = np.frombuffer(rgb.constBits(), dtype=np.uint8,
                        count=rgb.bytesPerLine() * rgb.height())
    px = buf.reshape(rgb.height(), rgb.bytesPerLine() // 4, 4)[
        int(pos.y()):int(pos.y()) + tl.height(),
        int(pos.x()):int(pos.x()) + tl.width()]
    r = px[:, :, 2].astype(int)
    g = px[:, :, 1].astype(int)
    b = px[:, :, 0].astype(int)
    return int(((b > r + 20) & (b > 90) & (r > 40)).sum())


def wb_top(w):
    while w.parent() is not None:
        w = w.parent()
    return w


def _png_equal(p, q):
    if not (os.path.exists(p) and os.path.exists(q)):
        return False
    with open(p, "rb") as a, open(q, "rb") as b:
        return a.read() == b.read()


def _run(m, wb, app, dst, changes):
    if os.path.exists(dst):
        os.remove(dst)
    for k, v in changes.items():
        if k == "mode":
            m.rb[v].setChecked(True)
        elif k == "loop":
            m.cb_loop.setChecked(v)
        else:
            getattr(m, WIDGET[k]).setValue(v)
    pump(app, 120)
    m.out_manual = True
    m.ed_out.setText(dst)
    m.run()
    assert wait_job(app, wb), f"配乐任务没在时限内结束：{dst}"
    m.cb_loop.setChecked(False)
    m.sp_delay.setValue(0.0)
    m.sl_gain.setValue(100)
    m.sl_orig.setValue(100)
    m.rb["replace"].setChecked(True)
    pump(app, 120)


def main():
    test_pure()
    if FF is None or not make_src():
        print("SKIP  没有可用的 ffmpeg，跳过配乐端到端测试")
        return
    ini = os.path.join(TMP, "settings_audio")
    shutil.rmtree(ini, ignore_errors=True)
    QtCore.QSettings.setDefaultFormat(QtCore.QSettings.Format.IniFormat)
    QtCore.QSettings.setPath(QtCore.QSettings.Format.IniFormat,
                             QtCore.QSettings.Scope.UserScope, ini)
    app = QtWidgets.QApplication(sys.argv)
    theme.apply(app)
    test_gui(app)
    print("\n全部通过" if not FAILS else f"\n失败 {len(FAILS)} 项：{'、'.join(FAILS)}")
    sys.exit(1 if FAILS else 0)


if __name__ == "__main__":
    main()
