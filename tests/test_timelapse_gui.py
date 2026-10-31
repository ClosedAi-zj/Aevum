# -*- coding: utf-8 -*-
"""延时视频 GUI（reference.gui.timelapse）端到端测试：序列识别 / 参数 -> 命令 /
单版与两版成片 / 缺口拒绝 / 取消清理。
运行：PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_timelapse_gui.py
"""
import os
import shutil
import sys
import tempfile
import time

import cv2
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "acceptance"))
from reference.ff import find_ffmpeg, probe_video     # noqa: E402
from reference.gui import timelapse as tg             # noqa: E402

FF = find_ffmpeg()
FAIL = []
TOTAL = [0]
SRC_IMG = os.path.join(ROOT, "aligned_lr3", "aligned_0500.jpg")


def check(name, cond, detail=""):
    TOTAL[0] += 1
    tag = "PASS" if cond else "FAIL"
    print(f"[{tag}] {name}  {detail}", flush=True)
    if not cond:
        FAIL.append(name)


def pump(app, ms):
    t0 = time.time()
    while time.time() - t0 < ms / 1000.0:
        app.update()
        time.sleep(0.02)


def wait_idle(app, timeout):
    t0 = time.time()
    while app.running and time.time() - t0 < timeout:
        app.update()
        time.sleep(0.05)
    return not app.running


def make_frames(d, n, w, h):
    im = cv2.imdecode(np.fromfile(SRC_IMG, dtype=np.uint8), cv2.IMREAD_COLOR)
    im = cv2.resize(im, (w, h), interpolation=cv2.INTER_AREA)
    for i in range(n):
        # 逐帧平移几像素，避免全静止画面把编码器"作弊"成极小体积
        m = np.float32([[1, 0, i % 5], [0, 1, 0]])
        f = cv2.warpAffine(im, m, (w, h), borderMode=cv2.BORDER_REFLECT_101)
        cv2.imwrite(os.path.join(d, f"clip_{i:04d}.jpg"), f,
                    [cv2.IMWRITE_JPEG_QUALITY, 92])


class Msgbox:
    """替换 messagebox：测试里弹窗会阻塞主线程。"""

    def __init__(self):
        self.calls = []
        self.answer = False

    def _rec(self, kind):
        def f(title, msg, *a, **kw):
            self.calls.append((kind, msg))
            return self.answer
        return f

    def install(self):
        tg.messagebox.showinfo = self._rec("info")
        tg.messagebox.showwarning = self._rec("warning")
        tg.messagebox.showerror = self._rec("error")
        tg.messagebox.askyesno = self._rec("askyesno")

    def last(self):
        return self.calls[-1][1] if self.calls else ""

    def clear(self):
        self.calls.clear()


def new_app(td):
    app = tg.TimelapseApp()
    app.withdraw()
    app.ent_dir.delete(0, "end")
    app.ent_dir.insert(0, td)
    app.detect()
    return app


def set_params(app, fps, aspect_text, side, codec_text, preset, crf, out):
    app.ent_fps.delete(0, "end"); app.ent_fps.insert(0, str(fps))
    app.cb_aspect.set(aspect_text)
    app.ent_side.delete(0, "end"); app.ent_side.insert(0, side)
    app.cb_codec.set(codec_text)
    app.cb_preset.set(preset)
    app.scl_crf.set(crf)
    app.out_manual = True
    app.ent_out.delete(0, "end"); app.ent_out.insert(0, out)
    app._refresh_labels()


def main():
    if not FF:
        print("SKIP: 没有 ffmpeg")
        return 1
    mb = Msgbox(); mb.install()
    td = tempfile.mkdtemp(prefix="tl_gui_")
    frames = os.path.join(td, "seq")
    os.makedirs(frames)
    make_frames(frames, 24, 1280, 800)
    outdir = os.path.join(td, "out")

    print("== 1. 序列识别 ==")
    app = new_app(frames)
    s = app.series
    check("识别张数", s and s["count"] == 24, str(s and s["count"]))
    check("识别编号范围", s and s["first"] == 0 and s["last"] == 23)
    check("模式补零宽度", s and s["pattern"] == "clip_%04d.jpg",
          s and s["pattern"])
    check("读取原图尺寸", s and (s["w"], s["h"]) == (1280, 800),
          s and f"{s['w']}x{s['h']}")
    check("信息栏显示张数", "24" in app.lbl_info.cget("text"),
          app.lbl_info.cget("text"))

    print("\n== 2. 不裁切单版 ==")
    set_params(app, 24, "原始比例（不裁切）", "640",
               "H.264 / x264（兼容性优先）", "ultrafast", 28,
               os.path.join(outdir, "single"))
    jobs = app._jobs()
    check("单版只出一个文件", len(jobs) == 1 and jobs[0][0] == "none", str(jobs))
    check("不裁切保持 16:10 比例", app._dims("none") == "640x400",
          app._dims("none"))
    app.run()
    check("单版跑完", wait_idle(app, 120), app.prog_lbl.cget("text"))
    p = jobs[0][1]
    info = probe_video(FF, p) if os.path.exists(p) else None
    check("单版成片存在", bool(info), p)
    if info:
        check("单版尺寸 640x400", (info["w"], info["h"]) == (640, 400),
              f'{info["w"]}x{info["h"]}')
        check("单版时长 1s", abs(info["dur"] - 1.0) < 0.1, f'{info["dur"]:.3f}s')

    print("\n== 3. 两版（16:9 + 原始） ==")
    set_params(app, 24, "16:9 + 原始（两版）", "640",
               "H.264 / x264（兼容性优先）", "ultrafast", 28,
               os.path.join(outdir, "both"))
    jobs = app._jobs()
    check("两版出两个文件", [a for a, _ in jobs] == ["16:9", "none"], str(jobs))
    check("两版文件名不冲突",
          os.path.basename(jobs[0][1]) == "both_16x9.mp4" and
          os.path.basename(jobs[1][1]) == "both_raw.mp4",
          " / ".join(os.path.basename(x[1]) for x in jobs))
    app.run()
    check("两版跑完", wait_idle(app, 180), app.prog_lbl.cget("text"))
    if os.path.exists(jobs[0][1]):
        i9 = probe_video(FF, jobs[0][1])
        check("16:9 版 640x360", (i9["w"], i9["h"]) == (640, 360),
              f'{i9["w"]}x{i9["h"]}')
    if os.path.exists(jobs[1][1]):
        ir = probe_video(FF, jobs[1][1])
        check("原始版 640x400", (ir["w"], ir["h"]) == (640, 400),
              f'{ir["w"]}x{ir["h"]}')

    print("\n== 3b. DCI 4K（4096x2160） ==")
    set_params(app, 24, "1.90:1  DCI 4K 4096x2160", "4096 (DCI 4K)",
               "H.264 / x264（兼容性优先）", "ultrafast", 28,
               os.path.join(outdir, "dci"))
    jobs = app._jobs()
    check("DCI 尺寸精确 4096x2160", app._dims("1.90:1") == "4096x2160",
          app._dims("1.90:1"))
    check("单比例不加后缀（两版才加）",
          os.path.basename(jobs[0][1]) == "dci.mp4", jobs[0][1])
    app.run()
    check("DCI 跑完", wait_idle(app, 240), app.prog_lbl.cget("text"))
    if os.path.exists(jobs[0][1]):
        idci = probe_video(FF, jobs[0][1])
        check("DCI 成片 4096x2160", (idci["w"], idci["h"]) == (4096, 2160),
              f'{idci["w"]}x{idci["h"]}')

    print("\n== 4. 逐帧进度 ==")
    txt = app.log_txt.get("1.0", "end")
    check("日志打印 ffmpeg 命令", "$ " in txt and "-progress pipe:1" in txt,
          "含 -progress pipe:1" if "-progress pipe:1" in txt else txt[-200:])
    check("进度条到头", app.prog.cget("value") == 1000,
          str(app.prog.cget("value")))
    check("打开输出按钮启用", str(app.open_btn.cget("state")) == "normal")

    print("\n== 5. 缺口报告与补位 ==")
    os.remove(os.path.join(frames, "clip_0010.jpg"))
    app.detect()
    check("缺口时 series 为空", app.series is None)
    check("缺口标红", "缺口" in app.lbl_info.cget("text"),
          app.lbl_info.cget("text"))
    mb.clear()
    app.run()
    check("缺口时拒绝合成", bool(mb.calls), mb.last())

    mb.answer = True
    mb.clear()
    app.detect()
    check("允许缺口后识别成功", app.series is not None)
    check("缺口清单正确", app.series and app.series["gaps"] == [10],
          str(app.series and app.series["gaps"]))
    check("张数按文件数", app.series and app.series["count"] == 23,
          str(app.series and app.series["count"]))
    set_params(app, 24, "原始比例（不裁切）", "640",
               "H.264 / x264（兼容性优先）", "ultrafast", 28,
               os.path.join(outdir, "gap"))
    jobs = app._jobs()
    app.run()
    check("缺口补位跑完", wait_idle(app, 120), app.prog_lbl.cget("text"))
    gi = probe_video(FF, jobs[0][1]) if os.path.exists(jobs[0][1]) else None
    check("缺口补一帧后时长不变", gi and abs(gi["dur"] - 1.0) < 0.1,
          gi and f'{gi["dur"]:.3f}s')
    check("临时补位目录已清理",
          not [n for n in os.listdir(td) if n.startswith(".lapse_gap_")],
          str(os.listdir(td)))

    print("\n== 6. 取消清理 ==")
    make_frames(frames, 120, 1280, 800)          # 补回缺口并加长序列
    app.detect()
    check("补齐后重新识别", app.series and app.series["count"] == 120,
          str(app.series and app.series["count"]))
    set_params(app, 120, "16:9 + 原始（两版）", "3840",
               "HEVC / x265（4K 高帧率必需）", "medium", 18,
               os.path.join(outdir, "cancel"))
    app.run()
    pump(app, 8000)
    prog = app.prog.cget("value")
    check("取消前进度在推进", prog > 0, f"{prog:.0f}/1000")
    app.cancel()
    check("取消后回到空闲", wait_idle(app, 60), app.prog_lbl.cget("text"))
    left = [p for _a, p in app._jobs() if os.path.exists(p)]
    check("半成品已删除", not left, str(left))
    check("取消后可再次开始", str(app.run_btn.cget("state")) == "normal")

    app.destroy()
    shutil.rmtree(td, ignore_errors=True)

    print(f"\n{'全部通过' if not FAIL else '失败: ' + ', '.join(FAIL)}"
          f"  （{TOTAL[0] - len(FAIL)}/{TOTAL[0]}）")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
