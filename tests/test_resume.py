# -*- coding: utf-8 -*-
"""断点续跑回归：批量导出中途取消后，下次只补没导出的帧。

O03 的验收点全在这里。为了不依赖真实解码速度，任务用 run() 同步跑：
进度信号是直连的，收到 15/30 就 cancel，下一帧循环头必然看到停止标志，
"停在第几张"是确定的，不是靠 sleep 猜的。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_resume.py
"""
import json
import os
import shutil
import sys
import time
import types

import cv2
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, ROOT)
TMP = os.path.join(HERE, "_tmp", "resume")
SHOTS = os.path.join(ROOT, "shots")

from PySide6 import QtCore, QtWidgets       # noqa: E402

from lapse.gui.grader import developer, renderq   # noqa: E402

FAILS = []
TOTAL = [0]


def check(name, ok, detail=""):
    TOTAL[0] += 1
    print(f"[{'PASS' if ok else 'FAIL'}] {name}  {detail}", flush=True)
    if not ok:
        FAILS.append(name)


def write_src(d, n=30):
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    paths = []
    for i in range(n):
        rng = np.random.default_rng(i)
        low = cv2.resize(rng.random((8, 12)).astype(np.float32),
                         (96, 64), interpolation=cv2.INTER_CUBIC) * 120
        im = np.clip(low + 60, 0, 255).astype(np.uint8)
        p = os.path.join(d, f"img_{i:04d}.jpg")
        cv2.imwrite(p, cv2.cvtColor(im, cv2.COLOR_GRAY2BGR),
                    [cv2.IMWRITE_JPEG_QUALITY, 95])
        paths.append(p)
    return paths


def fresh_out(d):
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    return d


def listed(out, ext):
    return sorted(f for f in os.listdir(out) if f.endswith(ext))


def progress_names(out):
    p = renderq.progress_path(out)
    if not os.path.isfile(p):
        return None
    with open(p, encoding="utf-8") as f:
        return sorted(json.load(f).get("done") or [])


def run_job(items, out, fmt="JPG", bit=8, stop_after=None, resume=True):
    """同步跑一批；stop_after=N 表示进度到第 N 张时点取消。返回 (job, 结果)。"""
    job = developer.BatchExport(items, out, developer.gc.defaults(),
                                95, fmt, bit, resume=resume)
    got = {}

    def on_progress(frac, detail):
        if stop_after and round(frac * len(items)) >= stop_after:
            job.cancel()

    def on_done(ok, res):
        got["ok"] = ok
        got["res"] = res

    if stop_after:
        job.progress.connect(on_progress)
    job.done.connect(on_done)
    job.run()
    return job, got


def test_cancel_then_resume():
    src = write_src(os.path.join(TMP, "src"))
    out = fresh_out(os.path.join(TMP, "out"))

    job, got = run_job(src, out, stop_after=15)
    check("取消在第 15 张停下", not got["ok"] and len(listed(out, ".jpg")) == 15,
          f"{len(listed(out, '.jpg'))} 张 / {got['res']}")
    check("取消说明还剩多少", "剩余 15 张" in str(got["res"]), str(got["res"]))
    names = progress_names(out)
    check("进度文件记下 15 个名字", names is not None and len(names) == 15,
          f"{len(names or [])}")
    check("进度记的是源文件名不是输出名",
          names == sorted(os.path.basename(p) for p in src[:15]),
          str(names)[:60])

    job2, got2 = run_job(src, out)
    check("续跑只补剩下的 15 帧", got2["ok"] and job2.skipped == 15,
          f"跳过 {job2.skipped}")
    check("最终 30 张齐全", len(listed(out, ".jpg")) == 30,
          f"{len(listed(out, '.jpg'))}")
    check("导完删掉进度文件", not os.path.isfile(renderq.progress_path(out)),
          str(progress_names(out)))
    check("续跑结果是全部输出", len(got2["res"]) == 30, f"{len(got2['res'])}")


def test_fmt_invalidates():
    src = write_src(os.path.join(TMP, "src2"))
    out = fresh_out(os.path.join(TMP, "out2"))
    run_job(src, out, fmt="PNG", stop_after=12)
    check("PNG 取消后留了进度", len(progress_names(out) or []) == 12,
          str(progress_names(out)))
    job, got = run_job(src, out, fmt="JPG")
    check("换格式进度作废、从头跑", got["ok"] and job.skipped == 0
          and len(listed(out, ".jpg")) == 30,
          f"跳过 {job.skipped}，jpg {len(listed(out, '.jpg'))}")
    check("换格式没把 png 当成品复用", len(listed(out, ".png")) == 12,
          f"png {len(listed(out, '.png'))} 张（仍是取消前那些）")
    # 位深同样是导出规格：16bit 的记录不能给 8bit 用
    run_job(src, out, fmt="PNG", bit=16, stop_after=9)
    job, _ = run_job(src, out, fmt="PNG", bit=8)
    check("换位深进度同样作废", job.skipped == 0, f"跳过 {job.skipped}")


def test_missing_file_reexported():
    src = write_src(os.path.join(TMP, "src3"))
    out = fresh_out(os.path.join(TMP, "out3"))
    run_job(src, out, stop_after=15)
    gone = os.path.join(out, "img_0007.jpg")
    os.remove(gone)
    job, got = run_job(src, out)
    check("手工删掉的那张重导", got["ok"] and os.path.isfile(gone)
          and job.skipped == 14, f"跳过 {job.skipped}")
    check("删一张后仍然 30 张齐全", len(listed(out, ".jpg")) == 30,
          f"{len(listed(out, '.jpg'))}")


def test_no_resume_and_bad_record():
    src = write_src(os.path.join(TMP, "src4"))
    out = fresh_out(os.path.join(TMP, "out4"))
    run_job(src, out, stop_after=20)
    check("每 20 帧落一次盘", len(progress_names(out) or []) >= 20,
          str(len(progress_names(out) or [])))
    job, got = run_job(src, out, resume=False)
    check("选「否」重新导出全部", got["ok"] and job.skipped == 0
          and len(listed(out, ".jpg")) == 30, f"跳过 {job.skipped}")

    bad = fresh_out(os.path.join(TMP, "out5"))
    with open(renderq.progress_path(bad), "w", encoding="utf-8") as f:
        f.write("{ 这不是 JSON")
    job, got = run_job(src, bad)
    check("坏进度文件不影响导出", got["ok"] and job.skipped == 0,
          f"跳过 {job.skipped}")
    check("坏进度文件被导完覆盖掉",
          not os.path.isfile(renderq.progress_path(bad)),
          str(progress_names(bad)))


def test_partial_failure_keeps_progress():
    """有帧失败（坏图）时：成功的算完成、进度留下，下次只补那颗坏帧。"""
    src = write_src(os.path.join(TMP, "src9"))
    junk = os.path.join(TMP, "src9", "broke.jpg")
    with open(junk, "wb") as f:
        f.write(b"not an image at all")
    out = fresh_out(os.path.join(TMP, "out9"))
    job, got = run_job(src + [junk], out)
    check("坏帧不拖死整批", not got["ok"] and len(got["res"]) == 30,
          f"成功 {len(got['res'] or [])} 张")
    check("有失败时进度仍记下", len(progress_names(out) or []) == 30,
          str(len(progress_names(out) or [])))
    job2, got2 = run_job(src + [junk], out)
    check("再跑只补那颗坏帧", job2.skipped == 30 and not got2["ok"],
          f"跳过 {job2.skipped}")

    msg = []
    ns = types.SimpleNamespace(status=msg.append)
    developer.Developer._batch_done(ns, False, got["res"], out, job)
    check("部分失败也说清进度已记下",
          "有帧失败" in (msg[0] if msg else "")
          and "进度已记下" in (msg[0] if msg else ""), " ".join(msg))


def test_ask_resume_dialog():
    """真弹窗：截图 + 点「继续」，验证提示文案与人选的两种走向。"""
    app = QtWidgets.QApplication.instance() or QtWidgets.QApplication(sys.argv)
    src = write_src(os.path.join(TMP, "src6"))
    out = fresh_out(os.path.join(TMP, "out6"))
    run_job(src, out, stop_after=15)
    host = QtWidgets.QWidget()
    host.resize(520, 260)
    host.setWindowTitle("断点续跑提示")
    host.show()
    app.processEvents()

    seen = {}

    def answer(click):
        """弹窗是模态阻塞的，所以先排一次定时器：出现就截图，然后点指定按钮。"""
        def fire():
            box = app.activeModalWidget()
            if box is None:
                return
            seen["text"] = " ".join(
                lb.text() for lb in box.findChildren(QtWidgets.QLabel)
                if lb.text())
            seen["buttons"] = " | ".join((b.text() or "")
                                         for b in box.buttons())
            os.makedirs(SHOTS, exist_ok=True)
            box.grab().save(os.path.join(SHOTS, "shot_断点续跑.png"))
            for b in box.buttons():
                if click in (b.text() or ""):
                    b.click()
                    return
            box.reject()
        QtCore.QTimer.singleShot(160, fire)

    ns = types.SimpleNamespace(shell=host)
    answer("继续导")
    yes = developer.Developer._ask_resume(ns, out, "JPG", 8, 30)
    app.processEvents()
    check("有进度时弹出询问", bool(seen.get("text")), str(seen.get("text"))[:60])
    check("文案报已完成与剩下", "已完成 15 帧" in seen.get("text", "")
          and "剩下的 15 帧" in seen.get("text", ""),
          seen.get("text", "").replace("\n", " ")[:90])
    check("按钮是人话不是 Yes/No",
          "继续导" in seen.get("buttons", "")
          and "全部重新导出" in seen.get("buttons", ""),
          seen.get("buttons", ""))
    check("点「继续导」返回 True", yes is True, str(yes))

    answer("全部重新导出")
    seen.clear()
    no = developer.Developer._ask_resume(ns, out, "JPG", 8, 30)
    app.processEvents()
    check("点「全部重新导出」返回 False", no is False, str(no))
    check("没有进度时不打扰",
          developer.Developer._ask_resume(ns, fresh_out(
              os.path.join(TMP, "out7")), "JPG", 8, 30) is True)
    host.close()


def test_resume_saves_half_the_work():
    """验收点 4：选继续 → 只补剩下的，活儿约为全量的一半。

    本机计时噪声大，所以只要求明显小于全量（<0.8×），不咬死 0.5×。
    """
    src = write_src(os.path.join(TMP, "src8"))
    full = fresh_out(os.path.join(TMP, "out8"))
    t0 = time.time()
    run_job(src, full)
    t_full = time.time() - t0

    part = fresh_out(os.path.join(TMP, "out8b"))
    run_job(src, part, stop_after=15)
    t0 = time.time()
    job, got = run_job(src, part)
    t_resume = time.time() - t0
    check("续跑只做剩下的一半活", got["ok"] and job.skipped == 15
          and t_resume < t_full * 0.8,
          f"全量 {t_full:.2f}s vs 续跑 {t_resume:.2f}s")


class _Signal:
    """给假 owner 用的最小信号替身：只要 emit 能转给一个函数。"""

    def __init__(self, fn):
        self.fn = fn

    def emit(self, *a):
        self.fn(*a)


class _Owner:
    """render_frames 只问三样：log、progress、stopping。

    stop_after=N：进度走到第 N 帧就翻停止标志，于是"停在第几张"是确定的。
    """

    stopping = False

    def __init__(self, logs, stop_after=0):
        self.stop_after = stop_after
        self.seen = 0
        self.log = _Signal(logs.append)
        self.progress = _Signal(self._on_progress)

    def _on_progress(self, frac, detail):
        self.seen += 1
        if self.stop_after and self.seen >= self.stop_after:
            self.stopping = True


def run_frames(src, out, p, sig=None, stop_after=0):
    """直接走 renderq.render_frames（渲染队列用的那条路），返回 (结果, 日志)。"""
    logs = []
    res = renderq.render_frames(_Owner(logs, stop_after), src, out, p, None,
                                95, "JPG", 8, True, sig)
    return res, logs


def test_sig_invalidates_old_frames():
    """渲染队列的续跑多一道锁：调色改过，上次的中间帧就不能接着拼新片。"""
    src = write_src(os.path.join(TMP, "src10"))
    out = fresh_out(os.path.join(TMP, "out10"))
    p0 = developer.gc.defaults()
    sig0 = renderq.sig_of(p0)
    res, _logs = run_frames(src, out, p0, sig=sig0, stop_after=12)
    check("带指纹的进度记下 12 帧", res.stopped == 18 and res.skipped == 0
          and len(progress_names(out) or []) == 12,
          f"跳过 {res.skipped}、停剩 {res.stopped}")

    res2, logs2 = run_frames(src, out, p0, sig=sig0)
    check("指纹对上：跳过已渲的 12 帧", res2.skipped == 12
          and len(res2.outs) == 30 and not res2.bad,
          f"跳过 {res2.skipped} 共 {len(res2.outs)}")
    check("渲完删掉进度", not os.path.isfile(renderq.progress_path(out)))

    p1 = dict(p0, exposure=1.0)
    res3, logs3 = run_frames(src, out, p1, sig=renderq.sig_of(p1))
    check("改了调色：旧中间帧全部作废", res3.skipped == 0
          and len(res3.outs) == 30, f"跳过 {res3.skipped}")
    check("作废时说一声为什么",
          any("重渲" in t for t in logs3), " / ".join(logs3)[:80])
    # 没给指纹的老调用（批量导出）行为不变：进度照旧能续
    plain = fresh_out(os.path.join(TMP, "out11"))
    renderq.flush_progress(plain, "JPG", 8, ["img_0001.jpg"], 30)
    check("不传指纹的老批量导出看不看 sig 都能续",
          renderq.read_progress(plain, "JPG", 8) is not None
          and renderq.read_progress(plain, "JPG", 8, "随便什么") is None,
          str(renderq.read_progress(plain, "JPG", 8)))


def main():
    QtWidgets.QApplication(sys.argv)
    test_cancel_then_resume()
    test_fmt_invalidates()
    test_missing_file_reexported()
    test_no_resume_and_bad_record()
    test_sig_invalidates_old_frames()
    test_resume_saves_half_the_work()
    test_partial_failure_keeps_progress()
    test_ask_resume_dialog()
    print(f"\n{TOTAL[0] - len(FAILS)}/{TOTAL[0]} 通过")
    if FAILS:
        print("失败：" + "、".join(FAILS))
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
