# -*- coding: utf-8 -*-
"""渲染队列：一次排若干段序列，逐段渲成"调色后的成片"。

队列里存的只是渲染意图（哪个目录、出到哪、成片规格）。每一段的参数、
关键帧、去闪烁、曝光台阶都在开跑前从**它自己的旁车**里现读，所以排完队
再回头改某一段的调色，不用重新排过。

中间调色帧落在成片旁边的 `<片名>_帧/`，断点续跑用和批量导出同一份
`.lapse_progress.json`；进度里带参数指纹，改过调色就自动作废重渲，
不会拿旧帧拼新片。
"""
import copy
import hashlib
import json
import os
import shutil
import time
from collections import namedtuple

import numpy as np

from reference import deflicker as flicker
from reference import ff as ffmod
from reference import grade as gc
from reference import sequence as seq
from reference import video
from reference.gui import edits, job as jobs, prefs
from reference.gui.grader import exporter

PROGRESS_NAME = ".lapse_progress.json"
PROGRESS_FLUSH = 20          # 每这么多帧落一次盘：崩溃最多重跑这些

VERSION = 1
QUEUE_NAME = "render_queue.json"
FRAME_FMT = "JPG"            # 中间帧格式：成片本来就是 8 位 yuv420p，这里不追高位深
FRAME_QUALITY = 95
FRAME_KEEP = 0.65            # 一项里"渲帧"占的进度权重，剩下的给编码
MAX_ROWS = 200

PENDING, DONE, FAILED = "待渲", "完成", "失败"
ASPECTS = tuple(video.ASPECT_CHOICES)
CODECS = ("hevc", "h264")

# 渲帧结果：outs 落盘成功的帧，bad 失败的源文件名，skipped 续跑跳过的，
# stopped 取消时剩下的帧数
FrameRun = namedtuple("FrameRun", "outs bad skipped stopped")


def _frac(i, n, p0, p1):
    """第 i 帧（从 1 数）在这段任务的全局进度里走到哪儿。"""
    return p0 + (p1 - p0) * (i / max(n, 1))

PATH = None                  # 测试用：指到别处就不碰用户那份队列


def path():
    if PATH:
        return PATH
    return os.path.join(prefs._roaming_root(), QUEUE_NAME)


def key_for(folder):
    """队列项和旁车之间唯一的对应关系：一个序列目录共用一套调色。"""
    return "series:" + os.path.normpath(str(folder))


def frames_dir(out):
    """中间调色帧的目录：跟着成片走，删成片时好一起找。"""
    return os.path.splitext(str(out))[0] + "_帧"


# ---------------------------------------------------------------------- 队列文件
def _count(v):
    """帧数只是列表里给人看一眼的，坏值当"未知"，不能因此丢掉整行。"""
    try:
        n = int(v or 0)
    except (TypeError, ValueError):
        return 0
    return max(n, 0)


def coerce(raw):
    """一行队列记录 → 干净字典；坏数据返回 None（宁可不显示，不能渲错）。"""
    if not isinstance(raw, dict):
        return None
    folder = os.path.normpath(str(raw.get("folder") or ""))
    out = os.path.normpath(str(raw.get("out") or ""))
    if not folder or not out:
        return None
    try:
        fps = float(raw.get("fps") or 0)
        side = int(raw.get("long_side") or 0)
        crf = int(raw.get("crf") or 18)
    except (TypeError, ValueError):
        return None
    if not 1 <= fps <= 1000 or side < 64 or not 0 <= crf <= 40:
        return None
    aspect = str(raw.get("aspect") or "16:9")
    if aspect not in ASPECTS:
        return None
    status = str(raw.get("status") or PENDING)
    return {
        "folder": folder,
        "out": out,
        "frames": _count(raw.get("frames")),
        "fps": fps,
        "long_side": side,
        "aspect": aspect,
        "codec": str(raw.get("codec") or "hevc")
                 if str(raw.get("codec") or "hevc") in CODECS else "hevc",
        "preset": ffmod.preset_value(raw.get("preset") or "fast"),
        "crf": crf,
        "keep_frames": bool(raw.get("keep_frames")),
        "status": status if status in (PENDING, DONE, FAILED) else PENDING,
        "note": str(raw.get("note") or ""),
    }


def load():
    """读队列；文件坏了就当空队列，绝不能拖垮启动。"""
    try:
        with open(path(), encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError):
        return []
    rows = data.get("items") if isinstance(data, dict) else None
    out = []
    for raw in rows or ():
        it = coerce(raw)
        if it:
            out.append(it)
    return out


def save(items):
    return edits.write_json(path(), {
        "version": VERSION,
        "saved": time.strftime("%Y-%m-%d %H:%M:%S"),
        "items": [dict(i) for i in (items or ())]})


def put(items, it):
    """同一目录只留一行：重复加入等于更新规格并退回待渲。"""
    key = os.path.normcase(it["folder"])
    rows = [r for r in items if os.path.normcase(r["folder"]) != key]
    rows.append(dict(it))
    return rows[-MAX_ROWS:]


# ---------------------------------------------------------------------- 断点续跑
def progress_path(out_dir):
    return os.path.join(out_dir, PROGRESS_NAME)


def read_progress(out_dir, fmt, bit, sig=None):
    """上次没导完的进度：{"done": [源文件名], "total": n}；不匹配或坏文件返回 None。

    导出规格（格式 / 位深）变了就不能复用，否则 done 里记的是 .png 的结果，
    这次要的却是 .jpg。sig 是参数指纹：渲染队列拿它判断"这些帧还是按现在
    这套调色渲出来的吗"，改了参数旧帧就不作数。
    """
    p = progress_path(out_dir)
    if not os.path.isfile(p):
        return None
    try:
        with open(p, encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError):
        return None
    if not isinstance(data, dict):
        return None
    if str(data.get("fmt", "")).upper() != str(fmt).upper():
        return None
    if int(data.get("bit") or 0) != int(bit):
        return None
    if sig is not None and str(data.get("sig") or "") != str(sig):
        return None
    done = [str(n) for n in (data.get("done") or []) if n]
    if not done:
        return None
    return {"done": done, "total": int(data.get("total") or 0)}


def clear_progress(out_dir):
    """删掉进度文件；用 .del 改名再删，Windows 上占用时不会留半个文件。"""
    p = progress_path(out_dir)
    if not os.path.isfile(p):
        return
    try:
        os.replace(p, p + ".del")
        os.remove(p + ".del")
    except OSError:
        pass


def flush_progress(out_dir, fmt, bit, done, total, sig=None):
    payload = {"version": 1, "fmt": str(fmt).upper(), "bit": int(bit),
               "total": int(total), "done": sorted(done),
               "updated": time.strftime("%Y-%m-%d %H:%M:%S")}
    if sig:
        payload["sig"] = str(sig)
    return edits.write_json(progress_path(out_dir), payload)


def sig_of(base, kfs=None, flick=None, holy=None):
    """给"这一批帧是按哪套参数渲的"起一个指纹：改了调色，旧帧就不作数。"""
    try:
        text = json.dumps({"p": base, "k": kfs, "f": flick, "h": holy},
                          sort_keys=True, default=str)
    except (TypeError, ValueError):
        text = repr(sorted((str(k), str(v)) for k, v in (base or {}).items()))
    return hashlib.sha1(text.encode("utf-8")).hexdigest()[:16]


# ---------------------------------------------------------------------- 逐帧参数
def flicker_ev(flick, index):
    """第 index 帧的去闪烁修正 (R,G,B)。

    标量记录（旧数据、纯亮度去闪）三个值相同；分通道记录各给各的。
    """
    zero = (0.0, 0.0, 0.0)
    v = _offset(flick, index)
    if v is None:
        return zero

    def clamp(x):
        return max(-flicker.MAX_EV, min(flicker.MAX_EV, float(x)))

    try:
        if isinstance(v, (list, tuple)) and len(v) == 3:
            return tuple(clamp(x) for x in v)
        s = clamp(v)
        return (s, s, s)
    except (TypeError, ValueError):
        return zero


def _offset(data, index):
    """旁车 offsets 里取第 index 个；越界或坏数据返回 None。"""
    if not isinstance(data, dict):
        return None
    try:
        i = int(index)
    except (TypeError, ValueError):
        return None
    offs = data.get("offsets") or []
    if not 0 <= i < len(offs):
        return None
    return offs[i]


def holy_offset(holy, index):
    v = _offset(holy, index)
    try:
        return float(v) if v is not None else 0.0
    except (TypeError, ValueError):
        return 0.0


def ev_into(p, index, flick=None, holy=None):
    """把第 index 帧的去闪烁 + 台阶曝光写进参数副本；没有修正返回原参数。

    三个通道修正相同就并进曝光（和旧标量行为逐位一致），只有分通道去闪真的
    让 R/G/B 各补各的时才挂 chan_ev，由 grade 的白平衡查表吃掉。
    """
    fev = flicker_ev(flick, index)
    flat = abs(fev[0] - fev[1]) < 1e-6 and abs(fev[1] - fev[2]) < 1e-6
    off = holy_offset(holy, index) + (fev[0] if flat else 0.0)
    if abs(off) < 1e-9 and flat:
        return p
    q = copy.deepcopy(p)
    q["exposure"] = float(q.get("exposure", 0.0)) + off
    if flat:
        q.pop("chan_ev", None)
    else:
        q["chan_ev"] = [fev[0], fev[1], fev[2]]
    return q


def params_at(frames, base, kfs=None, flick=None, holy=None):
    """逐帧参数表：按文件位置取关键帧插值，再叠去闪烁与台阶曝光。

    没有任何逐帧修正时返回 None —— 调用方据此走"整套参数"的路径。
    """
    kfs = list(kfs or [])
    if not kfs and not flick and not holy:
        return None
    table = {os.path.normcase(os.path.normpath(p)): i
             for i, p in enumerate(frames)}

    def at(src):
        i = table.get(os.path.normcase(os.path.normpath(src)))
        if i is None:
            return base
        p = seq.params_at(base, kfs, i) if kfs else base
        return ev_into(p, i, flick, holy)

    return at


# ---------------------------------------------------------------------- 渲帧
def source_paths(folder, s):
    """按序列编号列出真实存在的源文件；缺口自然跳过，不补位。

    编号顺序 ≠ 文件名排序：补零位数以识别序列时的 width 为准，
    这样和 ffmpeg 之后吃的那批帧一定是同一批、同一顺序。
    """
    ext = os.path.splitext(s["sample"])[1]
    out = []
    for n in range(int(s["first"]), int(s["last"]) + 1):
        p = os.path.join(folder, "%s%0*d%s" % (s["prefix"], int(s["width"]),
                                               n, ext))
        if os.path.isfile(p):
            out.append(p)
    return out


def render_frames(owner, items, out_dir, base, params_at, quality=FRAME_QUALITY,
                  fmt=FRAME_FMT, bit=8, resume=True, sig=None, detail="",
                  p0=0.0, p1=1.0):
    """逐帧调色落盘，返回 FrameRun。

    owner 只要提供 log / progress / stopping 三样（BatchExport 与渲染队列都
    满足），这样"批量导帧"和"队列渲片"共用同一套断点续跑与取消语义。
    detail 给每帧的进度文字加前缀（队列里要说"第 3/8 段"），p0/p1 是这段
    帧任务在**全局进度条**里占的区间 —— 队列一项有渲帧 + 编码两个阶段。
    """
    outs, bad = [], []
    done, skipped, since = set(), 0, 0
    n = len(items)
    ext = exporter.EXPORT_FORMATS.get(str(fmt).upper(), ".jpg")
    if resume:
        info = read_progress(out_dir, fmt, bit, sig)
        if info:
            done = set(info["done"])
            owner.log.emit("断点续跑：跳过已完成的 %d 帧" % len(done))
        elif sig is not None:
            # 参数指纹对不上：旧帧是新片不认的，整批作废重渲
            clear_progress(out_dir)
            owner.log.emit("调色参数和上次不同，中间帧全部重渲")
    else:
        clear_progress(out_dir)
    for i, src in enumerate(items):
        if owner.stopping:
            flush_progress(out_dir, fmt, bit, done, n, sig)
            return FrameRun(outs, bad, skipped, n - i)
        name = os.path.basename(src)
        dst = os.path.join(out_dir, os.path.splitext(name)[0] + ext)
        # 进度说渲过还不够，得文件真的在：手工删过的那张要重渲
        if name in done and os.path.isfile(dst):
            outs.append(dst)
            skipped += 1
            owner.progress.emit(_frac(i + 1, n, p0, p1),
                                f"{detail}{i + 1}/{n}（跳过）")
            continue
        try:
            im = exporter.load_full(src)
            if im is None:
                bad.append(name)
            else:
                p = params_at(src) if params_at else base
                out = np.clip(gc.apply(im, p), 0, 1)
                if exporter.save_image(dst, out, quality, bit):
                    outs.append(dst)
                    done.add(name)
                else:
                    bad.append(name)
        except Exception as e:
            # 单张坏图/坏参数不能拖死整批：记下来继续下一张
            bad.append(name)
            owner.log.emit(f"{name}: {e}")
        since += 1
        if since >= PROGRESS_FLUSH:
            since = 0
            flush_progress(out_dir, fmt, bit, done, n, sig)
        owner.progress.emit(_frac(i + 1, n, p0, p1), f"{detail}{i + 1}/{n}")
    if bad:
        owner.log.emit("失败 " + str(len(bad)) + " 张：" + "、".join(bad[:5]))
        flush_progress(out_dir, fmt, bit, done, n, sig)   # 留进度，下次只补这些
    else:
        clear_progress(out_dir)
    return FrameRun(outs, bad, skipped, 0)


# ---------------------------------------------------------------------- 编码
def encode_cmd(ff, src_dir, out, it, pattern, start, count, src_w, src_h,
               threads=0):
    """一段成片的 ffmpeg 命令：直接吃中间调色帧目录。"""
    return video.build_cmd(
        ff, src_dir, out, it["fps"],
        None if it["aspect"] == "none" else it["aspect"],
        it["long_side"], it["crf"], it["preset"], it["codec"],
        pattern, start, count, src_w, src_h, int(threads), False,
        progress=True)


class RenderQueue(jobs.Job):
    """按队列顺序把每一项渲成调色成片：先出调色帧，再交给 ffmpeg。

    plans 是主线程算好的（读旁车、认序列都在那儿做完），线程里只读文件、跑
    子进程，绝不碰任何控件 —— 跨线程读控件是这个项目踩过的老坑。
    某一项失败不拖死整队：记下原因，接着跑下一项。
    """

    def __init__(self, plans, ff, threads=0):
        super().__init__([])
        self.plans = list(plans)
        self.ff = ff
        self.threads = int(threads)
        self.results = []               # [(目录, 成功?, 说明, [成片])]

    def run(self):
        n = len(self.plans)
        if not n:
            self.done.emit(False, "队列是空的")
            return
        files, failed = [], 0
        for k, plan in enumerate(self.plans):
            if self.stopping:
                self.results.append((plan["item"]["folder"], False, "未开始", []))
                continue
            it = plan["item"]
            self.log.emit(f"\n[{k + 1}/{n}] {os.path.basename(it['folder'])}"
                          f" → {it['out']}")
            ok, note, outs = self._one(plan, k / n, 1.0 / n)
            self.results.append((it["folder"], ok, note, outs))
            files += outs
            if not ok:
                failed += 1
        if self.stopping:
            self.done.emit(False, "cancelled")
            return
        if failed:
            self.done.emit(False, f"{failed}/{n} 段没渲成")
        else:
            self.done.emit(True, files)

    def _one(self, plan, f0, span):
        it = plan["item"]
        fd = frames_dir(it["out"])
        try:
            os.makedirs(fd, exist_ok=True)
        except OSError as e:
            return False, f"建不出中间帧目录：{e}", []
        note = ""
        res = render_frames(
            self, plan["sources"], fd, plan["params"], plan["params_at"],
            quality=plan["quality"], fmt=FRAME_FMT, bit=8, sig=plan["sig"],
            detail=f"{plan['index']}/{plan['total']}段 帧 ",
            p0=f0, p1=f0 + span * FRAME_KEEP)
        if res.stopped:
            return False, "已取消（进度已记下，下次接着渲）", []
        if len(res.outs) < 2:
            return False, f"渲得出来的帧不足两张（失败 {len(res.bad)}）", []
        if res.bad:
            note = f"{len(res.bad)} 帧没渲出来，按上一帧停留 "
        self._prune(fd, res.outs)

        # 缺号在这里补位：ffmpeg 的 image2 遇到缺号会直接结束（实测只读出前半段）
        try:
            s = video.scan_series(fd, allow_gaps=True)
        except video.SeriesError as e:
            return False, note + f"调色帧目录读不出序列：{e}", []
        src_dir, pattern, start, count, cleanup = video.fill_gaps(fd, s)
        todo = video.outputs_for(it["aspect"], it["out"])
        e0 = f0 + span * FRAME_KEEP
        span_e = span * (1.0 - FRAME_KEEP) / max(len(todo), 1)
        made = []
        try:
            for j, (a, out) in enumerate(sorted(todo.items())):
                if self.stopping:
                    break
                od = os.path.dirname(out)
                if od:
                    os.makedirs(od, exist_ok=True)
                cmd, w, h = encode_cmd(self.ff, src_dir, out, it, pattern,
                                       start, count, plan["w"], plan["h"],
                                       self.threads)
                rc, misc = self._run_one(cmd, jobs.Meter("frame", count), None,
                                         e0 + j * span_e, span_e)
                if self.stopping:
                    break
                if rc != 0 or not os.path.exists(out):
                    for m in misc[-6:]:
                        self.log.emit(m)
                    self._drop(out)
                    return False, note + f"ffmpeg 退出码 {rc}", made
                made.append(out)
                self.log.emit(f"完成 {out}  "
                              f"{os.path.getsize(out) / 1e6:.1f} MB  {w}×{h}")
        finally:
            if cleanup:
                cleanup()
        if self.stopping:
            # 半成品不可播放，删掉；中间帧和进度留着，下次接着渲这一项
            for out in made:
                self._drop(out)
            return False, "已取消（进度已记下，下次接着渲）", []
        if not made:
            return False, note + "一段成片都没出来", made
        if it["keep_frames"]:
            note += f"中间帧留在 {os.path.basename(fd)}"
        else:
            self._drop_dir(fd)
            note += "中间帧已清"
        return True, note.strip(), made

    @staticmethod
    def _drop(out):
        try:
            if os.path.exists(out):
                os.remove(out)
        except OSError:
            pass

    @staticmethod
    def _prune(fd, outs):
        """删掉这一批之外的残留帧。

        上次渲了 1386 帧、这次源序列只剩 1200 帧时，多出来的那 186 张还挂在
        目录里，ffmpeg 会照着编号把它们编进片尾 —— 那是旧调色的尾巴。
        """
        want = {os.path.basename(p) for p in outs}
        want.add(PROGRESS_NAME)
        for name in os.listdir(fd):
            if name in want:
                continue
            p = os.path.join(fd, name)
            if os.path.isfile(p):
                RenderQueue._drop(p)

    def _drop_dir(self, d):
        """删中间帧目录：只删自己建的那个，删失败也只记日志。"""
        try:
            shutil.rmtree(d, ignore_errors=True)
            self.log.emit(f"已清掉中间帧 {d}")
        except OSError as e:
            self.log.emit(f"中间帧没清掉：{e}")
