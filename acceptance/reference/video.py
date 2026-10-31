# -*- coding: utf-8 -*-
"""延时照片序列 -> 视频：序列识别、目标尺寸、ffmpeg 命令拼装。

界面与命令行共用本模块；这里不碰窗口、不跑进程，只出参数和命令。
"""
import os
import re
import shutil
import tempfile

from reference import imio

# 1.90:1 用 DCI 4K 的精确比例定义，这样长边填 4096 正好得到 4096x2160；
# 消费级 UHD 4K 是 3840x2160（16:9）。
RATIOS = {"16:9": 9 / 16, "3:2": 2 / 3, "4:3": 3 / 4, "1.90:1": 2160 / 4096}
ASPECT_LABELS = {"none": "_raw", "16:9": "_16x9", "3:2": "_3x2",
                 "4:3": "_4x3", "1.90:1": "_DCI"}
ASPECT_CHOICES = list(RATIOS) + ["none", "both"]


class SeriesError(ValueError):
    """目录不是合法序列：没有图片、命名不符、有缺口或前缀不一致。"""


class SeriesGapError(SeriesError):
    """编号不连续。gaps 是缺失的编号，交给调用方决定是否放行。"""

    def __init__(self, gaps):
        self.gaps = list(gaps)
        s = self.gaps
        super().__init__(
            f"序列有 {len(s)} 处缺口，缺 {s[:10]}"
            f"{'…' if len(s) > 10 else ''}。\n"
            f"缺的那几帧会被跳过，成片看起来像抖动。\n"
            f"可以补齐或改名后重试，也可以允许缺口继续（缺口处按上一帧停留）。")


def even(v):
    v = int(round(v))
    return v - (v % 2)


def aspects_for(aspect):
    return ["16:9", "none"] if aspect == "both" else [aspect]


def out_label(aspect):
    return ASPECT_LABELS.get(aspect, "_" + aspect.replace(":", "x"))


def target_size(aspect, long_side, src_w=0, src_h=0):
    """该比例下的输出宽高（偶数）。aspect=None/none 表示按原图比例不裁切。"""
    if aspect in RATIOS:
        return int(long_side), even(long_side * RATIOS[aspect])
    if src_w:
        return int(long_side), even(long_side * src_h / float(src_w))
    return int(long_side), 0


def find_series(d, exts=imio.IMG_EXTENSIONS, allow_gaps=False):
    """返回 (前缀, 起始编号, 结束编号, 张数, 补零位数, 缺口列表)。

    缺口默认仍然拒绝（ffmpeg 会跳过缺的那几帧，成片看起来像抖动），
    但把缺口清单交给调用方，让它可以问用户「要不要按上一帧停留继续」。
    """
    names = [n for n in os.listdir(d)
             if os.path.splitext(n)[1].lower() in exts]
    if not names:
        raise SeriesError(f"目录里没有图片: {d}")
    parsed = []
    for n in names:
        m = re.match(r"^(.*?)(\d+)\.[A-Za-z0-9]+$", n)
        if not m:
            raise SeriesError(f"文件名不符合 前缀+编号 规则: {n}")
        parsed.append((m.group(1), int(m.group(2)), len(m.group(2))))
    nums = sorted(n for _, n, _ in parsed)
    gaps = sorted(set(range(nums[0], nums[-1] + 1)) - set(nums))
    if gaps and not allow_gaps:
        raise SeriesGapError(gaps)
    # 补零位数按文件名里的实际位数，不是数字本身长度：clip_0000..0023 是 4 位
    width = max(w for _p, _n, w in parsed)
    prefix = parsed[0][0]
    if len({p for p, _, _ in parsed}) != 1:
        raise SeriesError("目录内图片前缀不一致，无法作为同一序列")
    return prefix, nums[0], nums[-1], len(nums), width, gaps


def scan_series(d, allow_gaps=False):
    """识别序列，返回 dict：prefix/first/last/count/width/pattern/sample/gaps。"""
    prefix, first, last, count, width, gaps = find_series(
        d, allow_gaps=allow_gaps)
    sample = next(n for n in sorted(os.listdir(d))
                  if re.match(re.escape(prefix) + r"\d+\.[A-Za-z0-9]+$", n))
    ext = os.path.splitext(sample)[1]
    return {"prefix": prefix, "first": first, "last": last, "count": count,
            "width": width, "pattern": f"{prefix}%0{width}d{ext}",
            "sample": sample, "gaps": gaps}


def fill_gaps(src, s):
    """缺口处复制上一帧，返回 (目录, pattern, start, count, 清理函数)。

    ffmpeg 的 image2 解复用器遇到缺号会直接结束（实测 7.1：5 张缺 1 张只读出
    2 帧），所以「允许缺口」不能只是放行，必须先把缺号补上再交给 ffmpeg。
    镜像目录用硬链接（同盘即时完成），跨盘时退化为复制；没有缺口时直接返回
    原目录，清理函数为 None。
    """
    gaps = s.get("gaps") or []
    if not gaps:
        return src, s["pattern"], s["first"], s["count"], None

    ext = os.path.splitext(s["sample"])[1]
    pat = re.compile(re.escape(s["prefix"]) + r"(\d+)\.[A-Za-z0-9]+$")
    by_num = {}
    for name in os.listdir(src):
        m = pat.match(name)
        if m:
            by_num[int(m.group(1))] = name
    if len(by_num) != s["count"]:
        raise SeriesError(f"序列文件数与识别结果不符：{len(by_num)} vs {s['count']}")

    base = os.path.dirname(os.path.abspath(src.rstrip("\\/")))
    try:
        tmp = tempfile.mkdtemp(prefix=".lapse_gap_", dir=base)
    except OSError:
        tmp = tempfile.mkdtemp(prefix="lapse_gap_")
    try:
        prev = None
        for n in range(s["first"], s["last"] + 1):
            name = by_num.get(n)
            if name is not None:
                prev = name
            elif prev is None:
                raise SeriesError("缺口出现在首帧之前，无法用上一帧补位")
            src_path = os.path.join(src, prev)
            dst = os.path.join(tmp, f"{s['prefix']}{n:0{s['width']}d}{ext}")
            try:
                os.link(src_path, dst)
            except OSError:
                shutil.copy2(src_path, dst)
    except BaseException:
        shutil.rmtree(tmp, ignore_errors=True)
        raise
    count = s["last"] - s["first"] + 1
    return tmp, s["pattern"], s["first"], count, \
        lambda: shutil.rmtree(tmp, ignore_errors=True)


def build_cmd(ff, src, out, fps, aspect, long_side, crf, preset, codec,
              pattern, start, count, src_w, src_h, threads, quiet,
              progress=False, audio_src=None, audio_filter=None):
    """拼出 ffmpeg 命令，返回 (cmd, 输出宽, 输出高)。

    audio_src：帧序列本身没有声音（运镜把视频逐帧重画过），把原来的视频再接
    一路输入、只复制它的音轨，声音才不会在重画这一步丢掉。
    audio_filter：给了它说明声音要跟着倍速走，那就不能 copy，必须重编；
    同时加 -shortest，变速后的声画长度不一致时以画面为准截断。
    """
    vf = []
    if aspect:
        # 先按长边缩到目标尺寸并放大填满，再居中裁成目标比例
        w, h = target_size(aspect, long_side)
        vf.append(f"scale={w}:{h}:force_original_aspect_ratio=increase")
        vf.append(f"crop={w}:{h}")
    else:
        w, h = target_size(None, long_side, src_w, src_h)
        vf.append(f"scale={w}:{h}" if h else f"scale={w}:-2")

    cmd = [ff, "-y"]
    if quiet:
        cmd += ["-loglevel", "error", "-stats"]
    elif progress:
        # GUI 要逐帧进度：机器可读的进度写 stdout，关掉会刷屏的人类 stats
        cmd += ["-loglevel", "error", "-nostats", "-progress", "pipe:1"]
    cmd += ["-framerate", f"{fps:g}",
            "-start_number", str(start),
            "-i", os.path.join(src, pattern)]
    if audio_src:
        cmd += ["-i", audio_src, "-map", "0:v:0", "-map", "1:a?"]
        if audio_filter:
            cmd += ["-filter:a", audio_filter, "-c:a", "aac", "-b:a", "192k",
                    "-shortest"]
        else:
            cmd += ["-c:a", "copy"]
    if codec == "hevc":
        cmd += ["-c:v", "libx265", "-preset", preset, "-crf", str(crf),
                "-tag:v", "hvc1"]
    else:
        cmd += ["-c:v", "libx264", "-preset", preset, "-crf", str(crf)]
    cmd += ["-pix_fmt", "yuv420p", "-vf", ",".join(vf),
            "-frames:v", str(count), "-fps_mode", "cfr",
            "-movflags", "+faststart", out]
    if threads:
        cmd += ["-threads", str(threads)]
    return cmd, w, h


def default_out(src, prefix, fps, long_side):
    """默认输出名放在图片目录的上一级：…/aligned_lr3/ -> …/aligned_120fps_3840.mp4。"""
    return os.path.abspath(os.path.join(
        src.rstrip("/\\"), "..", f"{prefix.strip('_')}_{fps:g}fps_{long_side}.mp4"))


def outputs_for(aspect, out_or_stem):
    """给定比例与输出路径（两版时当无后缀的 stem 用），返回 {比例: 路径}。"""
    aspects = aspects_for(aspect)
    stem, suffix = os.path.splitext(os.path.abspath(out_or_stem))
    if len(aspects) == 1:
        return {aspects[0]: stem + (suffix or ".mp4")}
    return {a: stem + out_label(a) + (suffix or ".mp4") for a in aspects}
