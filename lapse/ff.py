# -*- coding: utf-8 -*-
"""ffmpeg 相关的共享工具：找可执行文件、跑命令、探测视频流信息。"""
import os
import re
import shutil
import subprocess
import threading
from collections import OrderedDict

NO_WINDOW = 0x08000000 if os.name == "nt" else 0

# chroma_format_idc -> (SubWidthC, SubHeightC)，SPS 裁切字段单位换算用
SUBWH = {0: (1, 1), 1: (2, 2), 2: (2, 1), 3: (1, 1)}

# 编码速度组合框：显示人话，值仍是 ffmpeg 的 preset 名
PRESET_LABELS = [
    ("最快（体积最大）", "ultrafast"),
    ("更快", "veryfast"),
    ("快速", "faster"),
    ("均衡", "fast"),
    ("慢速（更小）", "medium"),
    ("更慢", "slow"),
    ("极慢（压到最小）", "veryslow"),
]


def preset_value(label_or_value):
    """把界面标签或旧设置里的原值都还原成 ffmpeg preset。"""
    for label, value in PRESET_LABELS:
        if label_or_value in (label, value):
            return value
    return "medium"


def find_ffmpeg():
    """优先用 PATH 里的 ffmpeg，否则用 venv 内 imageio-ffmpeg 自带的。"""
    exe = shutil.which("ffmpeg")
    if exe:
        return exe
    try:
        import imageio_ffmpeg
        return imageio_ffmpeg.get_ffmpeg_exe()
    except Exception:
        return None


def run(cmd):
    return subprocess.run(cmd, capture_output=True, creationflags=NO_WINDOW)


def extract_frame(ff, src, t, out=None, dst=None):
    """抽一帧做预览。给 out 就写盘（调色台要拿它喂 cv2），不给就直接回 PNG 字节。

    dst=(宽,高) 时按显示尺寸解出来：非方形像素（SAR≠1:1）直接解会得到被压扁的
    画面，旋转元数据 ffmpeg 已经自己转，但尺寸仍要按长边换算一次。
    """
    cmd = [ff, "-y", "-hide_banner", "-loglevel", "error",
           "-ss", f"{t:.3f}", "-i", src, "-frames:v", "1"]
    if dst:
        cmd += ["-vf", f"scale={int(dst[0])}:{int(dst[1])}"]
    if out:
        r = run(cmd + [out])
        return r.returncode == 0 and os.path.exists(out)
    r = run(cmd + ["-f", "image2pipe", "-c:v", "png", "pipe:1"])
    return r.stdout or None


def _trace_sps(ff, path, pre):
    """用 trace_headers 把 SPS 里的字段读成 {名字: 整数}；pre 是限制读取的输入参数。"""
    t = run([ff, "-hide_banner", "-loglevel", "debug", *pre, "-i", path,
             "-map", "0:v:0", "-c", "copy", "-bsf:v", "trace_headers",
             "-f", "data", os.devnull])
    vals = {}
    for line in t.stderr.decode("utf-8", "replace").splitlines():
        mm = re.search(r"\]\s*\d+\s+(\S+)\s+[\s01]*=\s*(-?\d+)\s*$", line)
        if mm:
            vals[mm.group(1)] = int(mm.group(2))
    return vals


def probe_video(ff, path):
    """读取流信息 + 码流头（编码尺寸/既有裁切窗口/显示尺寸）；不是视频返回 None。

    `w`/`h` 一直是**存储像素**尺寸（裁切、导出、指标都按它算），显示尺寸另存
    `dw`/`dh`：非方形像素要按 SAR 拉宽，带旋转元数据的要先拉再交换长短边。
    """
    r = run([ff, "-hide_banner", "-i", path])
    err = r.stderr.decode("utf-8", "replace")

    m = re.search(r"Video: ([a-zA-Z0-9]+)", err)
    if not m:
        return None
    codec = m.group(1)
    m = re.search(r"Video:.*?(\d{2,5})x(\d{2,5})", err)
    if not m:
        return None
    w, h = int(m.group(1)), int(m.group(2))

    dur = 0.0
    m = re.search(r"Duration: (\d+):(\d+):(\d+(?:\.\d+)?)", err)
    if m:
        dur = int(m.group(1)) * 3600 + int(m.group(2)) * 60 + float(m.group(3))

    m = re.search(r"([\d.]+) fps", err)
    fps = float(m.group(1)) if m else 0.0

    m = re.search(r"Video:.*?, ([a-z0-9]+)\(", err)
    pixfmt = m.group(1) if m else ""

    rotated = ("displaymatrix" in err) or ("rotation of" in err)
    angle = 0
    m = re.search(r"rotation of (-?[\d.]+) degrees", err)
    if m:
        angle = int(round(float(m.group(1)))) % 360

    # 像素纵横比：[SAR 16:9 DAR 16:9]；探不到就按 1:1
    sar = (1, 1)
    m = re.search(r"\[SAR (\d+):(\d+)", err)
    if m and int(m.group(2)):
        sar = (int(m.group(1)), int(m.group(2)))
        # 缩放取整会留下 23749:23754 这种"几乎方形"的怪比值，按 1:1 处理，
        # 不然信息条写一堆噪声，比特流无损裁切也会被它误关掉
        if abs(sar[0] / sar[1] - 1.0) < 1 / 256:
            sar = (1, 1)
    dw, dh = w * sar[0] / sar[1], float(h)
    if angle in (90, 270):
        dw, dh = dh, dw
    dw, dh = max(1, int(round(dw))), max(1, int(round(dh)))

    # 码流头：编码尺寸 + 既有裁切（SPS conformance window / frame cropping）
    coded = (w, h)
    conf = (0, 0, 0, 0)  # (left, right, top, bottom) 像素
    if codec in ("h264", "hevc"):
        # 参数集就在开头几个访问单元里，先只 demux 0.2s；真没解出来再扫整段
        #（整段 demux 在几十 MB 以上要好几秒，导入时主线程就是卡在这）
        vals = _trace_sps(ff, path, ["-t", "0.2"])
        if vals.get("chroma_format_idc") is None:
            vals = _trace_sps(ff, path, [])
        try:
            if codec == "hevc":
                cw = vals.get("pic_width_in_luma_samples")
                chh = vals.get("pic_height_in_luma_samples")
                if cw and chh:
                    sub = SUBWH.get(vals.get("chroma_format_idc", 1), (2, 2))
                    coded = (cw, chh)
                    if vals.get("conformance_window_flag"):
                        conf = (vals.get("conf_win_left_offset", 0) * sub[0],
                                vals.get("conf_win_right_offset", 0) * sub[0],
                                vals.get("conf_win_top_offset", 0) * sub[1],
                                vals.get("conf_win_bottom_offset", 0) * sub[1])
            else:
                if "pic_width_in_mbs_minus1" in vals:
                    cw = (vals["pic_width_in_mbs_minus1"] + 1) * 16
                    hmul = 16 if vals.get("frame_mbs_only_flag", 1) else 32
                    chh = (vals["pic_height_in_map_units_minus1"] + 1) * hmul
                    sub = SUBWH.get(vals.get("chroma_format_idc", 1), (2, 2))
                    coded = (cw, chh)
                    if vals.get("frame_cropping_flag"):
                        conf = (vals.get("frame_crop_left_offset", 0) * sub[0],
                                vals.get("frame_crop_right_offset", 0) * sub[0],
                                vals.get("frame_crop_top_offset", 0) * sub[1],
                                vals.get("frame_crop_bottom_offset", 0) * sub[1])
        except Exception:
            pass

    crop_ok = (codec in ("h264", "hevc") and not rotated and sar == (1, 1)
               and coded[0] - conf[0] - conf[1] == w
               and coded[1] - conf[2] - conf[3] == h)

    return {"path": path, "codec": codec, "w": w, "h": h, "fps": fps,
            "dur": dur, "pixfmt": pixfmt, "rotated": rotated,
            "sar": sar, "angle": angle, "dw": dw, "dh": dh,
            "coded": coded, "conf": conf, "crop_ok": crop_ok,
            # 有没有自带音轨：配乐模块的"与原声混合"要先确认这一点
            "audio": bool(re.search(r"Audio: [a-zA-Z0-9]+", err))}


def disp_size(info):
    """按显示尺寸取 (宽, 高)：非视频字典（图片）与原样输出的视频都退回存储尺寸。"""
    if not info:
        return 0, 0
    return (int(info.get("dw") or info.get("w") or 0),
            int(info.get("dh") or info.get("h") or 0))


def disp_text(info):
    """信息条那一段分辨率文字：显示尺寸和存储尺寸不一致时把原因写清楚。"""
    w, h = int(info.get("w") or 0), int(info.get("h") or 0)
    dw, dh = disp_size(info)
    if (dw, dh) == (w, h):
        return f"{w}×{h}"
    why = []
    if info.get("angle"):
        why.append(f"旋转 {info['angle']}°")
    if tuple(info.get("sar") or (1, 1)) != (1, 1):
        why.append(f"像素 {info['sar'][0]}:{info['sar'][1]}")
    return (f"{dw}×{dh}（存储 {w}×{h}"
            + (" · " + " · ".join(why) if why else "") + "）")


def fit_long_size(w, h, long_side):
    """按长边缩一档，取偶数：ffmpeg 的编码器都要偶数尺寸。"""
    if w <= 0 or h <= 0:
        return 640, 360
    s = min(1.0, long_side / max(w, h))
    return (max(2, int(round(w * s / 2)) * 2),
            max(2, int(round(h * s / 2)) * 2))


# 导入一条视频要问 ffmpeg 两件事：读流信息、解一张预览帧。调色台、裁剪、运动、
# 配乐问的都是同一条片子，所以结果按文件指纹缓存起来，第一次由后台线程付钱。
BUNDLE_LONG = 2200        # 预览帧长边：正好是调色台工作图的尺寸
_BUNDLE_MAX = 4           # 留几条来回切的片子就够了，一帧 PNG 有 1~2MB
_bundle_lock = threading.Lock()
_bundles = OrderedDict()  # 文件指纹 -> {"info", "png", "t"}


def _fingerprint(path):
    try:
        st = os.stat(path)
    except OSError:
        return None
    return (os.path.normpath(path), st.st_mtime_ns, st.st_size)


def import_bundle(ff, path, progress=None, stopping=None):
    """一条视频的导入结果：{"info": 探测字典, "png": 预览帧字节, "t": 取帧时间}。

    缓存命中就直接回，没命中才现做——主线程调用时（命令行、插件、流水线回填）
    走的就是这条同步路径；界面导入用后台线程，把 progress/stopping 传进来。
    文件被重写（指纹变了）自动作废，不会拿旧帧冒充新片。
    """
    key = _fingerprint(path)
    if key is None:
        return None
    with _bundle_lock:
        hit = _bundles.get(key)
        if hit is not None:
            _bundles.move_to_end(key)
            return hit
    if progress:
        progress(0.05, "读取视频信息")
    info = probe_video(ff, path)
    if not info:
        return None
    t = (info["dur"] or 0.0) * 0.25
    png = None
    if not (stopping and stopping()):
        if progress:
            progress(0.35, "解出预览帧")
        png = extract_frame(ff, path, t,
                            dst=fit_long_size(*disp_size(info), BUNDLE_LONG))
    b = {"info": info, "png": png, "t": t}
    if png is None:
        # 取消停在半路：探测结果留着有用，但缺帧的半成品不进缓存，
        # 否则下次装载会一直拿到一张没有画面的"预览"
        return b
    with _bundle_lock:
        _bundles[key] = b
        while len(_bundles) > _BUNDLE_MAX:
            _bundles.popitem(last=False)
    return b


def video_info(ff, path):
    """四个工具都要的探测结果：从导入缓存拿，缓存没有就顺带做一次导入。"""
    b = import_bundle(ff, path)
    return b["info"] if b else None


def cached_bundle(path):
    """只问缓存，不动 ffmpeg：主线程判断"要不要起后台预取"用这个。"""
    key = _fingerprint(path)
    if key is None:
        return None
    with _bundle_lock:
        b = _bundles.get(key)
        if b is not None:
            _bundles.move_to_end(key)
        return b


def clear_bundles():
    """清空导入缓存（测试与"素材文件被外部改写"时用）。"""
    with _bundle_lock:
        _bundles.clear()


def probe_audio(ff, path):
    """读音频流信息（编码/时长/采样率/声道路数）；没有音频流返回 None。

    只有 mp4 成片时长会跟着容器走，单段配乐的时长得单独探出来才能算
    "比画面长多少、要不要截"。
    """
    err = run([ff, "-hide_banner", "-i", path]).stderr.decode(
        "utf-8", "replace")
    m = re.search(r"Audio: ([a-zA-Z0-9]+)", err)
    if not m:
        return None
    dur = 0.0
    d = re.search(r"Duration: (\d+):(\d+):(\d+(?:\.\d+)?)", err)
    if d:
        dur = int(d.group(1)) * 3600 + int(d.group(2)) * 60 + float(d.group(3))
    hz = re.search(r"(\d+) Hz", err)
    lay = re.search(r"Audio:.*?, (mono|stereo|[0-9.]+(?:\(.*?\))? ?ch)", err)
    layout = lay.group(1).replace(" ch", "ch") if lay else ""
    return {"path": path, "codec": m.group(1), "dur": dur,
            "sr": int(hz.group(1)) if hz else 0, "layout": layout}


def unique_path(path):
    """已存在则在文件名后加 _1、_2 …"""
    if not os.path.exists(path):
        return path
    stem, ext = os.path.splitext(path)
    i = 1
    while os.path.exists(f"{stem}_{i}{ext}"):
        i += 1
    return f"{stem}_{i}{ext}"


def clamp(v, lo, hi):
    return max(lo, min(hi, v))
