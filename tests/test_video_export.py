"""端到端：调色台导出调色视频 -> 抽帧 -> 逐个参数校验调色真的生效。

进度条与取消按钮在工作台外壳上，导出入口在调色台模块里。
"""
import os, re, shutil, subprocess, sys, time
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, ROOT)
TMP = os.path.join(HERE, "_tmp")
os.makedirs(TMP, exist_ok=True)
from PySide6 import QtCore, QtWidgets
import cv2

from lapse import grade as gc                   # noqa: E402
from lapse.ff import find_ffmpeg, probe_video   # noqa: E402
from lapse.gui import prefs, theme, workbench   # noqa: E402
from lapse.gui.grader import developer, exporter as gradex   # noqa: E402

SRC = os.path.join(ROOT, "aligned_lr3")
SRC_IMG = os.path.join(SRC, "aligned_0600.jpg")
SRC_IMG2 = os.path.join(SRC, "aligned_0900.jpg")
SRC_VID = os.path.join(TMP, "_t_src.mp4")
OUT_VID = os.path.join(TMP, "_t_graded.mp4")
fails = []


def check(name, ok, detail=""):
    print(("  OK  " if ok else " FAIL ") + name + ("  " + detail if detail else ""))
    if not ok:
        fails.append(name)


def pump(app, ms):
    t0 = time.time()
    while (time.time() - t0) * 1000 < ms:
        app.processEvents(); QtCore.QThread.msleep(3)


def run(cmd):
    return subprocess.run(cmd, capture_output=True)


def frame_at(ff, path, t):
    out = os.path.join(TMP, "_t_f.png")
    if os.path.exists(out):
        os.remove(out)
    run([ff, "-y", "-hide_banner", "-loglevel", "error", "-ss", f"{t}",
         "-i", path, "-frames:v", "1", out])
    im = cv2.imread(out)
    return None if im is None else cv2.cvtColor(im, cv2.COLOR_BGR2RGB).astype(np.float32)


def hi_pass(im):
    """拉普拉斯绝对值均值：细节 / 锐度 / 颗粒的能量。"""
    g = im[..., :3].mean(axis=2)
    k = (g[1:-1, 1:-1] * 4 - g[:-2, 1:-1] - g[2:, 1:-1]
         - g[1:-1, :-2] - g[1:-1, 2:])
    return float(np.abs(k).mean())


def corners(im, n=60):
    return float(np.concatenate([im[:n, :n].ravel(), im[:n, -n:].ravel(),
                                 im[-n:, :n].ravel(), im[-n:, -n:].ravel()]).mean())


def dark_br(im):
    """暗部 25% 像素的蓝减红：阴影染色/分离色调的指纹。"""
    g = im[..., :3].mean(axis=2)
    m = g <= np.percentile(g, 25)
    return float(im[..., 2][m].mean() - im[..., 0][m].mean())


def export(w, app, dst):
    """走调色台的 export_video，等它结束。返回 (导出期间进度条可见, 成片存在)。"""
    wb = w.shell
    QtWidgets.QFileDialog.getSaveFileName = staticmethod(lambda *a, **k: (dst, ""))
    if os.path.exists(dst):
        os.remove(dst)
    w.export_video()
    visible = wb.prog.isVisible()
    t0 = time.time()
    while wb._job is not None and wb._job.isRunning() and (time.time() - t0) < 240:
        pump(app, 80)
    pump(app, 300)
    return visible, os.path.exists(dst) and os.path.getsize(dst) > 1000


def main():
    ini = os.path.join(TMP, "_t_ini")
    shutil.rmtree(ini, ignore_errors=True)
    QtCore.QSettings.setDefaultFormat(QtCore.QSettings.Format.IniFormat)
    QtCore.QSettings.setPath(QtCore.QSettings.Format.IniFormat,
                             QtCore.QSettings.Scope.UserScope, ini)
    prefs.put("edits/dir", os.path.join(TMP, "_t_edits"))

    ff = find_ffmpeg()
    check("找到 ffmpeg", bool(ff), ff or "")

    # 0) 滤镜链约束：降噪只做空间域、颗粒用固定种子不沸腾
    td = os.path.join(TMP, "_t_chain")
    shutil.rmtree(td, ignore_errors=True)
    os.makedirs(td)
    p = {**gc.defaults(), "nr_luma": 40, "nr_color": 30, "grain": 60,
         "clarity": 60}
    cmd, _ = gradex.build_video_cmd(ff, SRC_VID, OUT_VID, p, 640, 360,
                                    25, 18, "fast", td)
    fc = cmd[cmd.index("-filter_complex") + 1]
    m = re.search(r"hqdn3d=([\d.]+):([\d.]+):([\d.]+):([\d.]+)", fc)
    check("视频降噪只做空间域", bool(m) and m.group(3) == "0" == m.group(4),
          m.group(0) if m else "没有 hqdn3d")
    check("视频颗粒固定种子", "allf=p" in fc and "all_seed=20240517" in fc)
    check("清晰度带中间调权重", "0.35+2.6" in fc)

    # 0b) M04：一个组挂 4 个组件，只写 1 个 LUT / 1 张权重图 / 1 条支路
    def mask_group(ncomp=1, uniq=False):
        g = gc.new_group("linear")
        cs = []
        for i in range(ncomp):
            c = gc.new_component("radial" if i % 2 else "linear")
            if uniq:                       # 几何必须不同，否则会命中 _GEOM 缓存
                c["x1"] = 0.10 + 0.13 * i
                c["y1"] = 0.18 + 0.06 * i
                c["x2"] = 0.72 + 0.01 * i
                c["y2"] = 0.42 + 0.03 * i
            c["blend"] = ("add" if i == 0
                          else ("subtract" if i % 2 else "intersect"))
            cs.append(c)
        g["components"] = cs
        g["params"] = {"exposure": 0.6}
        return g

    def build(p, w_, h_, sub, lum=None):
        d = os.path.join(TMP, sub)
        shutil.rmtree(d, ignore_errors=True)
        os.makedirs(d)
        t0 = time.perf_counter()
        cmd, _ = gradex.build_video_cmd(ff, SRC_VID, OUT_VID, p, w_, h_,
                                        25, 18, "fast", d, lum=lum)
        return (time.perf_counter() - t0) * 1000, cmd, d

    w4, h4 = 1920, 1080
    p4 = {**gc.defaults(), "masks": [mask_group(4, uniq=True)]}
    _, cmd4, d4 = build(p4, w4, h4, "_t_m04")
    written = sorted(f for f in os.listdir(d4)
                     if f.startswith(("grade_m", "mask")))
    check("4 组件只写一个 LUT 与权重图",
          written == ["grade_m0.cube", "mask0.png"], str(written))
    fc4 = cmd4[cmd4.index("-filter_complex") + 1]
    check("4 组件只加一条蒙版支路",
          fc4.count("alphamerge") == 1
          and all(t in fc4 for t in ("[ms0]", "[mg0]", "[ma0]", "[mm0]"))
          and "[ms1]" not in fc4)
    field = gc.mask_field(h4, w4, p4["masks"][0])
    png = cv2.imread(os.path.join(d4, "mask0.png"), cv2.IMREAD_GRAYSCALE)
    check("权重图与预览同一张场",
          png is not None
          and float(np.abs(png.astype(np.float32) / 255.0 - field).max()) < 2 / 255.0,
          (f"max Δ={float(np.abs(png.astype(np.float32) / 255.0 - field).max()) * 255:.2f}/255"
           if png is not None else "缺 mask0.png"))

    # 耗时：4 个不同几何的组件仍要各算一次权重场，被消掉的是每个组件
    # 各写一份 LUT/PNG/支路。以「4 个独立蒙版」为旧表示做对照，
    # 再确认 4 组件组没有掉进按组件数翻倍的量级。
    # 三种情形**轮流**各测三轮、取各自最小：挨个测完再比会把"上一个 case 刚把
    # 几何缓存与内存搅热"算到下一个头上（2026-10-23 实测：轮流 1.15×，挨个 2.2×，
    # 同一段代码），阈值本身没变，只是别再拿噪声当回归。
    cases = (("1组件", {**gc.defaults(), "masks": [mask_group(1, uniq=True)]}),
             ("4组件组", p4),
             ("4独立蒙版", {**gc.defaults(),
                            "masks": [mask_group(1, uniq=True) for _ in range(4)]}))
    for tag, p in cases:            # 各自先预热一遍，模拟改参数后重复导出
        build(p, w4, h4, "_t_m04_" + tag)
    times = {tag: [] for tag, _p in cases}
    for _round in range(3):
        for tag, p in cases:
            times[tag].append(build(p, w4, h4, "_t_m04_" + tag)[0])
    t1, t4, tsep = (min(times[tag]) for tag, _p in cases)
    perf = f"1组件 {t1:.0f}ms / 4组件组 {t4:.0f}ms / 4独立蒙版 {tsep:.0f}ms"
    check("4 组件导出不再按组件翻倍", t4 < tsep * 0.75, perf)
    check("4 组件与单组件同量级", t4 < t1 * 1.7, perf)

    # 亮度范围：导出权重图必须吃传进来的首帧亮度，而不是忽略 lum
    gr = gc.new_group("radial")
    gr["components"][0].update({"x1": 0.5, "y1": 0.5,
                                "x2": 2.5, "y2": 2.5, "feather": 0})
    gr["params"] = {"exposure": 0.6}
    gr["range"] = {"lum_on": 1, "lum_min": 0.2, "lum_max": 0.7,
                   "lum_feather": 0.1}
    pr = {**gc.defaults(), "masks": [gr]}
    check("导出侧识别亮度范围", gc.masks_use_range(pr))
    _, _, dr = build(pr, 640, 360, "_t_m04_r1",
                     lum=np.full((360, 640), 0.3, np.float32))
    _, _, dr2 = build(pr, 640, 360, "_t_m04_r2",
                      lum=np.full((360, 640), 1.0, np.float32))
    mr = cv2.imread(os.path.join(dr, "mask0.png"), cv2.IMREAD_GRAYSCALE)
    mr2 = cv2.imread(os.path.join(dr2, "mask0.png"), cv2.IMREAD_GRAYSCALE)
    check("导出权重图吃首帧亮度",
          mr is not None and mr2 is not None
          and mr.mean() > 200 and mr2.mean() < 20,
          f"暗帧 {mr.mean():.0f} / 亮帧 {mr2.mean():.0f}"
          if mr is not None and mr2 is not None else "缺 mask0.png")

    # 1) 造一个小源视频
    lst = os.path.join(TMP, "_t_list.txt")
    with open(lst, "w", encoding="utf-8") as f:
        for p in (SRC_IMG, SRC_IMG2):
            f.write(f"file '{os.path.abspath(p).replace(os.sep, '/')}'\n")
            f.write("duration 2\n")
        f.write(f"file '{os.path.abspath(SRC_IMG2).replace(os.sep, '/')}'\n")
    r = run([ff, "-y", "-hide_banner", "-loglevel", "error",
             "-f", "concat", "-safe", "0", "-i", lst,
             "-vf", "scale=640:360", "-r", "25", "-pix_fmt", "yuv420p", SRC_VID])
    check("生成源视频", r.returncode == 0 and os.path.exists(SRC_VID),
          r.stderr.decode("utf-8", "replace")[-200:] if r.returncode else "")

    app = QtWidgets.QApplication([])
    theme.apply(app)
    wb = workbench.Workbench([developer.Developer])
    wb.show(); pump(app, 300)
    w = wb.module(developer.Developer.name)
    w.load_video(SRC_VID); pump(app, 3000)
    check("视频已探测", w.video is not None, str(w.video) if w.video else "")
    if not w.video:
        return finish(wb)

    t = min(1.0, w.video["dur"] * 0.4)
    base = frame_at(ff, SRC_VID, t)
    check("源帧抽取", base is not None)
    if base is None:
        return finish(wb)

    # 2) 每个参数单独导出一次，只验证它自己的效应
    cases = [
        ("色温", {"temperature": 90},
         lambda g: (g[..., 0].mean() > base[..., 0].mean() * 1.05
                    and g[..., 2].mean() < base[..., 2].mean() * 0.92),
         lambda g: f"ΔR={g[...,0].mean()-base[...,0].mean():+.1f} "
                   f"ΔB={g[...,2].mean()-base[...,2].mean():+.1f}"),
        ("晕影压暗边缘", {"vignette": 80},
         lambda g: corners(g) < corners(base) - 8,
         lambda g: f"角 {corners(base):.0f} -> {corners(g):.0f}"),
        ("清晰度", {"clarity": 80},
         lambda g: hi_pass(g) > hi_pass(base) * 1.25,
         lambda g: f"细节 {hi_pass(base):.1f} -> {hi_pass(g):.1f}"),
        ("锐化", {"sharp_amount": 100},
         lambda g: hi_pass(g) > hi_pass(base) * 1.12,
         lambda g: f"细节 {hi_pass(base):.1f} -> {hi_pass(g):.1f}"),
        ("颗粒", {"grain": 90},
         lambda g: hi_pass(g) > hi_pass(base) * 1.10,
         lambda g: f"细节 {hi_pass(base):.1f} -> {hi_pass(g):.1f}"),
        ("阴影提升", {"shadows": 80},
         lambda g: np.percentile(g, 10) > np.percentile(base, 10) + 6,
         lambda g: f"10%位 {np.percentile(base,10):.0f} -> {np.percentile(g,10):.0f}"),
        ("饱和度", {"saturation": 70},
         lambda g: g.std() > base.std() + 2,
         lambda g: f"σ {base.std():.1f} -> {g.std():.1f}"),
        ("阴影染色", {"grade_sh_h": 210, "grade_sh_s": 65},
         lambda g: dark_br(g) > dark_br(base) + 2,
         lambda g: f"暗部 B-R {dark_br(base):+.1f} -> {dark_br(g):+.1f}"),
    ]
    seen_progress = False
    for name, params, pred, desc in cases:
        w.store.reset_all()
        for k, v in params.items():
            w.store.set(k, v)
        pump(app, 250)
        vis, ok = export(w, app, OUT_VID)
        seen_progress = seen_progress or vis
        if not ok:
            check(name + " 导出", False)
            continue
        g = frame_at(ff, OUT_VID, t)
        check(name, pred(g), desc(g))
    check("导出时进度条可见", seen_progress)

    # 2b) M04 等价性：整幅蒙版曝光 +1，抽帧应与预览逐像素同源（< 2/255）
    gm = gc.new_group("radial")
    gm["components"][0].update({"x1": 0.5, "y1": 0.5,
                                "x2": 2.5, "y2": 2.5, "feather": 0})
    gm["params"] = {"exposure": 1.0}
    w.store.reset_all()
    w.store.p["masks"] = [gm]
    w.store.changed.emit("test")
    pump(app, 250)
    _, ok = export(w, app, OUT_VID)
    if not ok:
        check("蒙版导出与预览亮度一致", False, "导出失败")
    else:
        gf = frame_at(ff, OUT_VID, t)
        prev = np.clip(gc.apply(base / 255.0, w.store.p), 0, 1) * 255.0
        cy, cx = base.shape[0] // 2, base.shape[1] // 2
        sl = (slice(cy - 40, cy + 40), slice(cx - 40, cx + 40))
        d = abs(float(gf[sl].mean() - prev[sl].mean()))
        check("蒙版导出与预览亮度一致", d < 2.0, f"Δ={d:.2f}/255")

    # 3) 元数据保持
    w.store.reset_all()
    w.store.set("temperature", 40)
    pump(app, 250)
    export(w, app, OUT_VID)
    meta = probe_video(ff, OUT_VID)
    check("分辨率保持", meta["w"] == w.video["w"] and meta["h"] == w.video["h"],
          f"{meta['w']}x{meta['h']}")
    check("帧率保持", abs(meta["fps"] - w.video["fps"]) < 0.5, f"{meta['fps']}fps")
    check("时长保持", abs(meta["dur"] - w.video["dur"]) < 0.3,
          f"{meta['dur']:.2f}s vs {w.video['dur']:.2f}s")
    info = run([ff, "-hide_banner", "-i", OUT_VID]).stderr.decode(
        "utf-8", "replace")
    vline = next((s for s in info.splitlines() if "Video: hevc" in s), "")
    check("色彩标记 BT.709", "(tv, bt709," in vline, vline.strip()[-120:])
    out = frame_at(ff, OUT_VID, t)
    check("无高光削顶", out is not None and float((out >= 250).mean()) < 0.05,
          f"{float((out >= 250).mean())*100:.2f}%" if out is not None else "")
    check("导出结束 UI 复位", not wb.prog.isVisible() and w.btn_vid.isEnabled())

    # 4) 取消：用真正的大素材（1386 帧 4K）测。小片段往往在按下取消前就编完了，
    #    留下的是成品而不是半成品，清理逻辑就测不到。
    big = os.path.join(ROOT, "aligned_120fps_3840.mp4")
    dst = os.path.join(TMP, "_t_cancel.mp4")
    if os.path.exists(big):
        if os.path.exists(dst):
            os.remove(dst)
        w.load_video(big)
        pump(app, 1500)
        QtWidgets.QFileDialog.getSaveFileName = staticmethod(
            lambda *a, **k: (dst, ""))
        w.export_video()
        t0 = time.time()
        while wb.prog.value() == 0 and (time.time() - t0) < 60:
            pump(app, 50)
        check("取消前进度在推进", 0 < wb.prog.value() < 1000, str(wb.prog.value()))
        wb.cancel_job()
        t0 = time.time()
        while wb._job.isRunning() and (time.time() - t0) < 60:
            pump(app, 50)
        pump(app, 400)      # 线程结束不等于 done 信号已投递，UI 复位要再转一圈
        check("取消后任务已停", not wb._job.isRunning())
        check("取消后不留半成品", not os.path.exists(dst))
        check("取消后 UI 复位", not wb.prog.isVisible() and w.btn_vid.isEnabled())
    else:
        print("  --  跳过取消测试（缺少 4K 素材 aligned_120fps_3840.mp4）")

    return finish(wb)


def finish(wb):
    wb.close()
    print("\n" + ("全部通过" if not fails else f"失败 {len(fails)}: {fails}"))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
