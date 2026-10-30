# -*- coding: utf-8 -*-
"""播放引擎回归：图片序列与视频的取帧、定位、限速、收尾。

这里只测 playback 自己（不起整个界面）：素材现造，320x180、24 帧，
跑得快，出问题也能一眼看出是解码还是节奏控制。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_playback.py
"""
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, ROOT)
TMP = os.path.join(HERE, "_tmp")
os.makedirs(TMP, exist_ok=True)

import numpy as np                       # noqa: E402
from PySide6 import QtCore, QtWidgets     # noqa: E402

from lapse import ff as ffmod            # noqa: E402
from lapse.gui import playback           # noqa: E402

W, H, N, FPS = 320, 180, 24, 24
CLIP = os.path.join(TMP, "_play.mp4")
SEQ = [os.path.join(TMP, f"_play_{i:03d}.png") for i in range(N)]
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
        QtCore.QThread.msleep(3)
    app.processEvents()


def wait_until(app, cond, limit=8):
    """等状态成立：播放到尽头这类事只取决于解码快慢，写死毫秒会在慢机器上假失败。"""
    t0 = time.time()
    while not cond() and time.time() - t0 < limit:
        pump(app, 80)
    return cond()


def frames():
    """每帧一个唯一的灰度值，定位对不对直接看数值。"""
    for i in range(N):
        yield np.full((H, W, 3), 10 + i * 8, dtype=np.uint8)


def build_assets(ff):
    import cv2
    for i, f in enumerate(frames()):
        cv2.imwrite(SEQ[i], cv2.cvtColor(f, cv2.COLOR_BGR2RGB))
    if not os.path.exists(CLIP):
        cmd = [ff, "-y", "-loglevel", "error", "-f", "rawvideo",
               "-pix_fmt", "rgb24", "-s", f"{W}x{H}", "-r", str(FPS),
               "-i", "-", "-c:v", "libx264", "-pix_fmt", "yuv420p", CLIP]
        with subprocess.Popen(cmd, stdin=subprocess.PIPE) as p:
            for f in frames():
                p.stdin.write(f.tobytes())
        if not os.path.exists(CLIP):
            raise SystemExit("测试素材生成失败")


def mean_of(arr):
    return float(arr.reshape(-1, 3).mean(axis=1).mean())


def test_list_source():
    src = playback.ListSource(SEQ, long_side=160)
    check("序列长度", src.count == N and src.fps == 25.0, f"{src.count} 张")
    r = src.open(5)
    a = r.read()
    check("序列按索引起读", a is not None and abs(mean_of(a) - (10 + 5 * 8)) < 3,
          f"第 5 张均值 {mean_of(a):.1f}")
    r.seek(20)
    b = r.read()
    check("序列定位", abs(mean_of(b) - (10 + 20 * 8)) < 3, f"均值 {mean_of(b):.1f}")
    check("序列缩放", b.shape[:2] == (90, 160), f"{b.shape[1]}x{b.shape[0]}")
    bad = SEQ[:3] + [os.path.join(TMP, "_play_missing.png")] + SEQ[3:]
    r2 = playback.ListSource(bad).open(2)
    c = r2.read()
    check("坏帧跳过不卡住", c is not None, "读到下一张能读的")
    r3 = src.open(N - 1)
    r3.read()
    check("读到尽头返回 None", r3.read() is None)


def test_video_source(ff):
    info = ffmod.probe_video(ff, CLIP)
    src = playback.VideoSource(ff, CLIP, info)
    check("视频帧数", src.count >= N - 1 and abs(src.fps - FPS) < 1,
          f"{src.count} 帧 {src.fps}fps")
    r = src.open(0)
    first = r.read()
    check("视频出帧", first is not None and first.shape[2] == 3,
          "" if first is None else f"{first.shape[1]}x{first.shape[0]}")
    for _ in range(5):
        r.read()                     # 顺序读到第 5 帧，指针在 6
    proc0 = r.proc
    r.seek(8)                        # 只往前 2 帧：丢帧接着读，不值得重开
    check("短距离丢帧不重开", r.proc is proc0 and r.i >= 8, f"i={r.i}")
    r.seek(N - 2)                    # 大跳：重启 ffmpeg 快进过去
    check("大跳重开解码", r.proc is not proc0)
    check("跳到尾部仍能出帧", r.read() is not None)
    r.close()
    check("关闭进程", r.proc is None)


def test_player(app, ff):
    info = ffmod.probe_video(ff, CLIP)
    p = playback.Player()
    ended = []
    p.ended.connect(lambda: ended.append(p.index))
    got = []
    p.frame.connect(lambda i, a: got.append(i))

    # prime=False：只放指针，不起线程（载入静图时不该抢先解一帧）
    p.set_source(playback.VideoSource(ff, CLIP, info), 4, prime=False)
    pump(app, 200)
    check("载入不起线程", p._thread is None and not got and p.index == 4,
          f"index {p.index}")

    p.loop = False                   # 先不循环：900ms 够跑完这一段，绕回 0 就不是在测 advance
    p.play()
    pump(app, 900)
    p.pause()
    check("播放逐帧送达", len(got) > 3 and got[-1] > got[0] and p.index > 4,
          f"{len(got)} 帧，停在 {p.index}")
    check("播放中线程在跑", p._thread is not None)

    got.clear()
    p.seek(1)
    pump(app, 500)
    # 停手后最后一帧必须正好是定位到的那张；前面允许多送一帧在途旧帧
    check("拖动定位出帧", 1 in got and got[-1] == 1, f"送出 {got[:4]}")

    p.seek(N - 3)
    p.play()                         # 交给时钟记账：欠几帧就解几帧，到头发 ended
    pump(app, 700)
    check("放到头收笔", ended and not p.playing, f"ended={ended}")

    n_thread = p._thread
    p.shutdown()
    check("收尾停线程", p._thread is None and n_thread is not None)

    # 序列同样走这条控制线
    q = playback.Player()
    q.set_source(playback.ListSource(SEQ), 2, prime=False)
    frames2 = []
    q.frame.connect(lambda i, a: frames2.append(i))
    q.loop = False
    q.play()
    pump(app, 900)
    check("序列按序播放", len(frames2) >= 3 and frames2[0] == 2,
          f"送出 {frames2[:6]}")
    # 走到尽头要自己停：这取决于解码快慢，只能等状态，不能写死毫秒
    check("放到尽头就停", wait_until(app, lambda: not q.playing),
          f"停在 {q.index}/{q.count}")

    # 循环播放：走到尽头要自己回到 0 继续，不能停下
    q.loop = True
    mark = len(frames2)
    q.park(q.count - 2)                 # 只剩两张，很快就能撞到尽头
    q.play()
    wait_until(app, lambda: 0 in frames2[mark:])
    check("循环回到开头", 0 in frames2[mark:] and q.playing,
          f"送出 {frames2[mark:mark + 4]}")
    q.shutdown()


def test_pacing(app, ff):
    """速率 = min(帧率, 解码速率)：欠账最多 2 帧，解码慢时丢帧而不是攒帧。"""
    info = ffmod.probe_video(ff, CLIP)
    p = playback.Player()
    got = []
    p.frame.connect(lambda i, a: got.append(i))
    p.set_source(playback.VideoSource(ff, CLIP, info), 0, prime=False)
    p.set_speed(4.0)                      # 96fps 的时钟，4K 之外的素材一般跟得上
    p.play()
    pump(app, 1000)
    p.shutdown()
    check("倍速时钟生效", p.interval() == max(10, round(1000 / (info['fps'] * 4))),
          f"{len(got)} 帧 / 1s，间隔 {p.interval()}ms")
    # 循环回到 0 是唯一的断档来源：其余必须逐帧连续，说明没有攒过期帧
    check("帧号连续", all(b - a == 1 or b == 0 for a, b in zip(got, got[1:])),
          f"{got[:8]}")


def test_no_console():
    """窗口版 exe 里，少写一个 creationflags 就是一个常驻的黑控制台。

    这条不测行为、只扫源码：exe 是 --windowed 的，父进程没有控制台，
    任何不带 NO_WINDOW 的子进程都会自己开一个（用户看到的症状是
    "一放视频就冒出个黑窗"）。以后谁再加一处 Popen/run 忘了写就当场失败。
    """
    import ast
    bad = []
    for dirpath, dirs, files in os.walk(os.path.join(ROOT, "lapse")):
        dirs[:] = [d for d in dirs if d != "__pycache__"]
        for fn in files:
            if not fn.endswith(".py"):
                continue
            p = os.path.join(dirpath, fn)
            tree = ast.parse(open(p, encoding="utf-8").read(), p)
            for node in ast.walk(tree):
                f = node.func if isinstance(node, ast.Call) else None
                if (f is None or not isinstance(f, ast.Attribute)
                        or f.attr not in ("Popen", "run", "call",
                                          "check_output", "check_call")
                        or not isinstance(f.value, ast.Name)
                        or f.value.id != "subprocess"):
                    continue
                if not any(k.arg == "creationflags" for k in node.keywords):
                    bad.append(f"{os.path.relpath(p, ROOT)}:{node.lineno}")
    check("lapse/ 里每个子进程都关掉了控制台", not bad, "、".join(bad))


def main():
    app = QtWidgets.QApplication(sys.argv)
    test_no_console()
    ff = ffmod.find_ffmpeg()
    if not ff:
        print("未找到 ffmpeg，跳过播放测试")
        return 0
    build_assets(ff)
    test_list_source()
    test_video_source(ff)
    test_player(app, ff)
    test_pacing(app, ff)
    print("\n" + ("全部通过" if not FAILS else f"失败 {len(FAILS)}: {FAILS}"))
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
