"""导出：静态图 / 批量 / 视频（等效滤镜 + 3D LUT 保证所见即所得）/ .cube。
"""
import os

import cv2
import numpy as np

from reference import grade as gc
from reference import imio

EXPORT_FORMATS = {"JPG": ".jpg", "PNG": ".png", "TIFF": ".tif"}


def build_video_cmd(ff, src, out, p, w, h, fps, crf, preset, td, lum=None):
    """返回 (cmd, cwd)。视频的清晰度/纹理/颗粒/晕影用等效滤镜近似预览效果，
    颜色部分走 65^3 LUT；局部蒙版每组再加一条「LUT + 权重图」支路，
    用 overlay 按权重把两套结果混起来 —— 和预览 blend 的算法同一套。
    lum 是源视频首帧的亮度图（float32）：只有亮度范围蒙版需要它。
    必须在 cwd=td 下运行：lut3d 用相对路径，Windows 盘符冒号会破坏滤镜参数解析。"""
    gc.write_cube(os.path.join(td, "grade.cube"), p, size=65)
    clarity = p.get("clarity") or 0
    texture = p.get("texture") or 0
    grain = p.get("grain") or 0
    vig = p.get("vignette") or 0
    nr_l, nr_c = p.get("nr_luma") or 0, p.get("nr_color") or 0
    sharp = p.get("sharp_amount") or 0

    # 蒙版：一组一条支路。组内所有形状先合成为单张权重图再写盘 ——
    # 按组件写的话 4 个组件就是 4 张 4K 权重图 + 4 条 overlay 支路。
    # 只有空间项（锐化/降噪）的组没有逐点增量，不必开 LUT 支路。
    mw, mh = w - w % 2, h - h % 2
    masks = [m for m in gc.active_masks(p)
             if any(k in gc.MASK_KEYS and abs(float(v)) > 1e-6
                    for k, v in (m.get("params") or {}).items())]
    for i, m in enumerate(masks):
        gc.write_cube(os.path.join(td, f"grade_m{i}.cube"),
                      gc.mask_params(p, m), size=65)
        # 亮度范围蒙版没法只靠几何 PNG 表达：合成后的场已经含亮度项。
        # 视频每帧亮度不同，这里用调用方给的首帧亮度算一张静态权重图。
        field = gc.mask_field(mh, mw, m, lum=lum)
        if field is None:
            continue
        g = (np.clip(field, 0.0, 1.0) * 255).astype(np.uint8)
        cv2.imencode(".png", g)[1].tofile(os.path.join(td, f"mask{i}.png"))

    rate = f"{(fps or 25.0):.6f}"
    cmd = [ff, "-y", "-hide_banner", "-loglevel", "error",
           "-progress", "pipe:1", "-nostats", "-i", src]
    nxt = 1                                        # 下一个附加输入的序号
    idx_vig = idx_mask = None
    if vig:
        # 负暗角是"提亮四角"，用同一张压暗蒙版反算亮度（A*(2-B)），保留旧公式
        vp = p if vig > 0 else {**p, "vignette": abs(vig)}
        vg = gc.vignette_mask(w, h, vp)
        mask = (np.clip(vg, 0, 1) * 255).astype(np.uint8)
        cv2.imencode(".png", mask)[1].tofile(os.path.join(td, "vig.png"))
        cmd += ["-loop", "1", "-framerate", rate, "-i", "vig.png"]
        idx_vig = nxt
        nxt += 1
    if masks:
        idx_mask = nxt
        for i in range(len(masks)):
            cmd += ["-loop", "1", "-framerate", rate, "-i", f"mask{i}.png"]
            nxt += 1

    chain = []
    step = "[0:v]"
    if w % 2 or h % 2:
        chain.append(f"[0:v]crop={mw}:{mh}[ce]")
        step = "[ce]"
    if nr_l or nr_c:
        # 只开 hqdn3d 的空间项：时间项会把相邻帧的细节混进来，延时序列里
        # 云、车流、星轨都会被拖成鬼影（预览也不做时间域降噪）。
        ls = 1.5 + nr_l / 100.0 * 8.0
        cs = 1.5 + nr_c / 100.0 * 10.0
        chain.append(f"{step}hqdn3d={ls:.2f}:{cs:.2f}:0:0[dn]")
        step = "[dn]"
    if clarity:
        sigma = max(1.0, min(mw, mh) / 40.0)
        amt = clarity / 100.0 * 1.5
        chain.append(f"{step}gblur=sigma={sigma:.2f}[gb]")
        # 中间调权重与预览的 clarity 一致：高光/阴影少加，减少边缘光晕
        chain.append(f"{step}[gb]blend=all_expr="
                     f"'A+(A-B)*{amt:.4f}*(0.35+2.6*(A/255)*(1-A/255))'[cl]")
        step = "[cl]"
    if texture:
        sigma = max(0.5, min(mw, mh) / 500.0)
        amt = texture / 100.0 * 0.9
        chain.append(f"{step}gblur=sigma={sigma:.2f}[tb]")
        chain.append(f"{step}[tb]blend=all_expr='A+(A-B)*{amt:.4f}'[tx]")
        step = "[tx]"
    step += "lut3d=grade.cube:interp=tetrahedral"

    # 蒙版支路：主图与「叠了增量的图」都转成 rgb，按权重图 overlay 混起来
    for i in range(len(masks)):
        chain.append(f"{step},format=rgb24[bs{i}]")
        chain.append(f"[0:v]format=rgb24,lut3d=grade_m{i}.cube"
                     f":interp=tetrahedral,format=rgba[ms{i}]")
        chain.append(f"[{idx_mask + i}:v]format=gray[mg{i}]")
        chain.append(f"[ms{i}][mg{i}]alphamerge[ma{i}]")
        chain.append(f"[bs{i}][ma{i}]overlay=format=rgb:shortest=1"
                     f":x=0:y=0[mm{i}]")
        step = f"[mm{i}]format=rgb24"

    tail = []
    if sharp:
        r = float(p.get("sharp_radius") or 1.0)
        k = 3 if r < 1.2 else (5 if r < 2.2 else 7)
        amt = min(1.5, sharp / 100.0 * 1.2)
        tail.append(f"unsharp={k}:{k}:{amt:.3f}:{k}:{k}:0")
    if grain:
        # p = 固定图案噪声（不随时间变化），种子与预览的固定颗粒一致：
        # t+u 每帧重新随机，播放时颗粒会"沸腾"，延时片尤其明显。
        tail.append(f"noise=alls={int(round(grain / 100.0 * 19))}"
                    f":allf=p:all_seed=20240517")
    if tail:
        # step 的约定是"标签 + 滤波器串"，结尾不能是裸标签：
        # 写成 [tp],format=… ffmpeg 会报 No such filter: ''
        step += "," + ",".join(tail)
    if vig:
        # blend 的表达式按 0-255 原始值计算，必须 /255 归一
        expr = "A*B/255" if vig > 0 else "A*(510-B)/255"
        chain.append(step + "[lu]")
        chain.append(f"[{idx_vig}:v]format=rgb24[mk]")
        chain.append(f"[lu][mk]blend=all_expr='{expr}':shortest=1,"
                     f"format=yuv420p[v]")
    else:
        chain.append(step + ",format=yuv420p[v]")
    cmd += ["-filter_complex", ";".join(chain),
            "-map", "[v]", "-map", "0:a?",
            "-c:v", "libx265", "-crf", str(crf), "-preset", preset,
            "-tag:v", "hvc1",
            # 调色与 LUT 都在 sRGB 里做，成片明确标成 BT.709：
            # 不标的话播放器会按容器/解码器的默认猜，同一支片在别的软件里偏色。
            # libx265 不认通用 -color_primaries/-color_trc，VUI 得单独传，
            # 否则 ffmpeg 读回来是 bt709/unknown/unknown。
            "-x265-params",
            "colorprim=bt709:transfer=bt709:colormatrix=bt709:range=limited",
            "-colorspace", "bt709", "-color_primaries", "bt709",
            "-color_trc", "bt709", "-color_range", "tv",
            "-c:a", "copy",
            "-movflags", "+faststart", out]
    return cmd, td


def save_image(path, arr_f32, quality=95, bit=8):
    """arr_f32: float32 RGB [0,1]。写盘支持中文路径。

    bit=16 只在 PNG/TIFF 里有效；JPG 没有 16bit 编码，自动落回 8bit。
    """
    ext = os.path.splitext(path)[1].lower() or ".jpg"
    if bit == 16 and ext in (".png", ".tif", ".tiff"):
        arr = (np.clip(arr_f32, 0, 1) * 65535.0 + 0.5).astype(np.uint16)
        bgr = cv2.cvtColor(arr, cv2.COLOR_RGB2BGR)
        params = ([cv2.IMWRITE_PNG_COMPRESSION, 3]
                  if ext == ".png" else [])
    else:
        bgr = cv2.cvtColor(
            (np.clip(arr_f32, 0, 1) * 255.0 + 0.5).astype(np.uint8),
            cv2.COLOR_RGB2BGR)
        params = ([cv2.IMWRITE_JPEG_QUALITY, int(quality)]
                  if ext in (".jpg", ".jpeg") else
                  [cv2.IMWRITE_PNG_COMPRESSION, 3])
    try:
        ok, buf = cv2.imencode(ext, bgr, params)
        if ok:
            buf.tofile(path)
        return bool(ok)
    except Exception:
        return False


def load_full(path):
    """全分辨率解码为 float32 RGB [0,1]：RAW/16bit 源按 16bit 读，
    导出不再从 8bit 起步。"""
    try:
        im = imio.decode_image(path, bps=16, half_size=False)
    except Exception:
        return None
    if im is None:
        return None
    scale = 65535.0 if im.dtype == np.uint16 else 255.0
    return cv2.cvtColor(im, cv2.COLOR_BGR2RGB).astype(np.float32) / scale


def load_small(path, max_edge=1024):
    """缩略图级别的 float32 RGB [0,1]：给联系表这种"看趋势不看细节"的活。

    先解码成 8bit 再等比缩小，比按全分辨率调色快一个量级；颗粒和晕影是按
    比例算的，缩小后观感仍然对得上，但别拿它当成品交付。读不出返回 None。
    """
    try:
        im = imio.decode_image(path, bps=8, half_size=False)
    except Exception:
        return None
    if im is None:
        return None
    if im.ndim == 2:
        im = cv2.cvtColor(im, cv2.COLOR_GRAY2BGR)
    elif im.shape[2] == 4:
        im = cv2.cvtColor(im, cv2.COLOR_BGRA2BGR)
    h, w = im.shape[:2]
    m = max(h, w)
    limit = max(16, int(max_edge))
    if m > limit:
        s = limit / m
        im = cv2.resize(im, (max(1, int(round(w * s))),
                             max(1, int(round(h * s)))),
                        interpolation=cv2.INTER_AREA)
    return cv2.cvtColor(im, cv2.COLOR_BGR2RGB).astype(np.float32) / 255.0


def _fit(arr, w, h):
    """等比缩放后居中贴进 w×h 的黑框（float32 RGB [0,1]）。"""
    src_h, src_w = arr.shape[:2]
    s = min(w / max(src_w, 1), h / max(src_h, 1))
    nw, nh = max(1, int(round(src_w * s))), max(1, int(round(src_h * s)))
    small = cv2.resize(arr, (nw, nh), interpolation=cv2.INTER_AREA)
    box = np.zeros((h, w, 3), np.float32)
    y0, x0 = (h - nh) // 2, (w - nw) // 2
    box[y0:y0 + nh, x0:x0 + nw] = small
    return box


SHEET_GAP = 8                # 格子间距
SHEET_DIV = 2                # 每格里"原图 | 调色后"那条分隔线的宽度
SHEET_LABEL = 18             # 每格下面那条帧号的高度


def contact_sheet(pairs, labels=(), cell=380, cols=3, gap=SHEET_GAP):
    """把若干对 (原图, 调色后) 拼成一张前后对照的联系表。

    每格左半边是原图、右半边是调色后，中间一条白线分开 —— 并排不画分隔线
    会被看成一张宽图。行高按**第一张**的长宽比算：延时序列本来就是同一规格，
    逐格算高低会参差，反而看不出亮度趋势。
    返回 uint8 RGB，标签用 ASCII（cv2 的 putText 没有中文字形）。
    """
    n = len(pairs)
    if n == 0:
        return None
    cols = max(1, min(int(cols), n))
    rows = (n + cols - 1) // cols
    cell = max(32, int(cell))
    gap = max(1, int(gap))
    label_h, div = SHEET_LABEL, SHEET_DIV
    h0, w0 = pairs[0][0].shape[:2]
    ch = max(24, int(round(cell * h0 / max(w0, 1))))
    cw = 2 * cell + div
    sheet = np.zeros((rows * (ch + label_h + gap) + gap,
                      cols * (cw + gap) + gap, 3), np.float32)
    for k, (before, after) in enumerate(pairs):
        r, c = divmod(k, cols)
        y = gap + r * (ch + label_h + gap)
        x = gap + c * (cw + gap)
        sheet[y:y + ch, x:x + cell] = _fit(before, cell, ch)
        sheet[y:y + ch, x + cell + div:x + cell + div + cell] = _fit(after, cell, ch)
        sheet[y:y + ch, x + cell:x + cell + div] = 1.0
    out = (np.clip(sheet, 0, 1) * 255.0 + 0.5).astype(np.uint8)
    for k in range(n):
        if k >= len(labels) or not labels[k]:
            continue
        r, c = divmod(k, cols)
        y = gap + r * (ch + label_h + gap) + ch + 13
        x = gap + c * (cw + gap)
        cv2.putText(out, str(labels[k]), (x, y), cv2.FONT_HERSHEY_SIMPLEX,
                    0.42, (235, 235, 235), 1, cv2.LINE_AA)
    return out


def sample_indices(total, want):
    """从 total 帧里均匀取 want 个下标（含首尾），去重后按序返回。

    首尾一定要取到：延时序列的变化几乎都藏在开头和结尾。
    """
    total, want = int(total), int(want)
    if total <= 0 or want <= 0:
        return []
    if want >= total:
        return list(range(total))
    if want == 1:
        return [0]
    step = (total - 1) / (want - 1)
    return sorted({min(total - 1, int(round(i * step)))
                   for i in range(want)})

