# -*- coding: utf-8 -*-
"""视频无损裁剪：选区几何 + ffmpeg 命令，纯函数、不含界面。

三种输出模式：
  copy     直接改写 H.264/HEVC 码流里的 SPS 裁切窗口，不重新编码；
  lossless x265 lossless 重编码，逐像素一致；
  crf      x265 CRF 重编码，体积小、肉眼不可分辨。

选区一律是"显示尺寸"下的源像素矩形 (x, y, w, h)，并且宽高与左上角全取偶：
4:2:0 色度抽样要求裁切边界成对，取奇数会被 ffmpeg 静默挪一像素。
界面（lapse/gui/cropper.py）只做坐标换算，
几何规则都集中在这里，改一处两边都生效。

最后一节是 Ken Burns 运镜：取景框随时间插值，逐帧重画成一段视频。
"""
import cv2
import numpy as np

from lapse.ff import clamp

ASPECTS = [("自由", None), ("16:9", (16, 9)), ("3:2", (3, 2)), ("4:3", (4, 3)),
           ("1:1", (1, 1)), ("9:16", (9, 16)), ("2.4:1", (12, 5))]

MODES = (
    ("copy", "比特流无损",
     "不重新编码，只改写码流里的裁切窗口：画质 100% 无损、秒级完成、体积不变"
     "（仅 H.264 / HEVC 可用）"),
    ("lossless", "数学无损",
     "x265 lossless 重编码：逐像素一致，体积可能显著增大"),
    ("crf", "视觉无损",
     "x265 CRF 重编码：体积小，肉眼不可分辨（推荐日常）"),
)


def aspect_of(name):
    """按显示名取比例，取不到按"自由"处理。"""
    for n, a in ASPECTS:
        if n == name:
            return a
    return None


def even(v):
    return int(v) - int(v) % 2


def norm_rect(x0, y0, x1, y1, W, H):
    """整理选区：排序、夹在画面内、全部取偶（4:2:0 兼容）。返回 (x,y,w,h)。"""
    x0, x1 = sorted((int(round(x0)), int(round(x1))))
    y0, y1 = sorted((int(round(y0)), int(round(y1))))
    w = clamp(x1 - x0, 2, W)
    h = clamp(y1 - y0, 2, H)
    w -= w & 1
    h -= h & 1
    x = clamp(x0, 0, W - w)
    y = clamp(y0, 0, H - h)
    x -= x & 1
    y -= y & 1
    if x + w > W:
        w = W - x
        w -= w & 1
    if y + h > H:
        h = H - y
        h -= h & 1
    return x, y, w, h


def center_rect(W, H, aspect):
    """按比例在画面里放一个居中的最大选区 —— 切比例后先给用户看到的东西。"""
    if aspect:
        num, den = aspect
        k = max(2, int(min(W / num, H / den) / 2) * 2)
        w, h = num * k, den * k
    else:
        w, h = W - W % 2, H - H % 2
    return norm_rect((W - w) / 2, (H - h) / 2, (W + w) / 2, (H + h) / 2, W, H)


def refit_rect(sel, W, H, aspect):
    """已有选区换比例：绕中心缩放，不把它甩到别处去。"""
    x, y, w, h = sel
    if not aspect:
        return norm_rect(x, y, x + w, y + h, W, H)
    num, den = aspect
    cx, cy = x + w / 2, y + h / 2
    t = w / num if num >= den else h / den
    t = min(t, W / num, H / den)
    k = max(2, int(t / 2) * 2)
    w2, h2 = num * k, den * k
    return norm_rect(cx - w2 / 2, cy - h2 / 2, cx + w2 / 2, cy + h2 / 2, W, H)


def draw_rect(anchor, px, py, W, H, aspect):
    """空白处拖出新选区。定比例时以按下点为角，取两轴里更大的那个推进。"""
    ax, ay = anchor
    if not aspect:
        return norm_rect(ax, ay, px, py, W, H)
    num, den = aspect
    dx, dy = px - ax, py - ay
    t = max(abs(dx) / num, abs(dy) / den)
    max_t = min(W / num, H / den)
    max_t = min(max_t, (W - ax) / num if dx >= 0 else ax / num)
    max_t = min(max_t, (H - ay) / den if dy >= 0 else ay / den)
    k = max(2, int(min(t, max_t) / 2) * 2)
    w, h = num * k, den * k
    x = ax if dx >= 0 else ax - w
    y = ay if dy >= 0 else ay - h
    return clamp(int(x), 0, W - w), clamp(int(y), 0, H - h), w, h


def move_rect(sel, x, y, W, H):
    """框内拖动：只平移，左上角取偶。"""
    _x, _y, w, h = sel
    x = clamp(int(round(x)), 0, W - w)
    y = clamp(int(round(y)), 0, H - h)
    return x - x % 2, y - y % 2, w, h


def resize_rect(handle, orig, px, py, W, H, aspect):
    """拖 8 个把手之一改大小。定比例时以对角为锚点，短边跟着长边走。"""
    ox, oy, ow, oh = orig
    px = clamp(int(round(px)), 0, W)
    py = clamp(int(round(py)), 0, H)
    has_n = handle in ("nw", "n", "ne")
    has_s = handle in ("sw", "s", "se")
    has_w = handle in ("nw", "w", "sw")
    has_e = handle in ("ne", "e", "se")

    if aspect is None:
        w = ox + ow - px if has_w else (px - ox if has_e else ow)
        h = oy + oh - py if has_n else (py - oy if has_s else oh)
        w = max(2, min(w, W))
        h = max(2, min(h, H))
        w -= w % 2
        h -= h % 2
        x = ox + ow - w if has_w else ox
        y = oy + oh - h if has_n else oy
    else:
        num, den = aspect
        if handle in ("n", "s"):
            base = oy + oh if has_n else oy
            t = (base - py) / den if has_n else (py - oy) / den
            cx = ox + ow // 2
            max_t = 2 * min(cx, W - cx) / num
            max_t = min(max_t, base / den if has_n else (H - oy) / den)
            k = max(2, int(min(max(t, 0), max_t) / 2) * 2)
            w, h = num * k, den * k
            x = cx - w // 2
            y = base - h if has_n else oy
        elif handle in ("e", "w"):
            base = ox + ow if has_w else ox
            t = (base - px) / num if has_w else (px - ox) / num
            cy = oy + oh // 2
            max_t = 2 * min(cy, H - cy) / den
            max_t = min(max_t, base / num if has_w else (W - ox) / num)
            k = max(2, int(min(max(t, 0), max_t) / 2) * 2)
            w, h = num * k, den * k
            x = base - w if has_w else ox
            y = cy - h // 2
        else:
            ax = ox + ow if has_w else ox
            ay = oy + oh if has_n else oy
            wd = ax - px if has_w else px - ax
            hd = ay - py if has_n else py - ay
            t = max(wd / num, hd / den)
            max_t = min(ax / num if has_w else (W - ax) / num,
                        ay / den if has_n else (H - ay) / den)
            k = max(2, int(min(max(t, 0), max_t) / 2) * 2)
            w, h = num * k, den * k
            x = ax - w if has_w else ax
            y = ay - h if has_n else ay
    x = clamp(int(x), 0, W - w)
    y = clamp(int(y), 0, H - h)
    return x - x % 2, y - y % 2, w, h


def nudge_rect(sel, dx, dy, step, W, H):
    """方向键微调：一格 2 像素（保证仍是偶数），Shift 加速。"""
    x, y, w, h = sel
    return (clamp(x + dx * step, 0, W - w),
            clamp(y + dy * step, 0, H - h), w, h)


def hit_test(rect, px, py, corner=8, edge=5):
    """PS 风格命中检测：角 > 边 > 内部 > 外部。rect 为屏幕坐标 (x0,y0,x1,y1)。"""
    x0, y0, x1, y1 = rect
    for name, cx, cy in (("nw", x0, y0), ("ne", x1, y0),
                         ("se", x1, y1), ("sw", x0, y1)):
        if abs(px - cx) <= corner and abs(py - cy) <= corner:
            return "corner:" + name
    if abs(py - y0) <= edge and x0 - edge <= px <= x1 + edge:
        return "edge:n"
    if abs(py - y1) <= edge and x0 - edge <= px <= x1 + edge:
        return "edge:s"
    if abs(px - x0) <= edge and y0 - edge <= py <= y1 + edge:
        return "edge:w"
    if abs(px - x1) <= edge and y0 - edge <= py <= y1 + edge:
        return "edge:e"
    if x0 < px < x1 and y0 < py < y1:
        return "in"
    return "out"


def copy_supported(info):
    """模式 1 要求 H.264/HEVC、无旋转元数据，且 SPS 里读得到裁切窗口。"""
    return bool(info) and bool(info.get("crop_ok"))


def build_copy_cmd(ff, src, dst, info, sel):
    """模式1：流复制 + 改写码流 SPS 裁切字段（比特级无损）。sel 为显示坐标。

    源本身可能已经带一个裁切窗口（conf），新选区是在显示画面上量的，
    所以要把原窗口偏移加回去，否则会在已裁的基础上再裁错一格。
    """
    x, y, w, h = sel
    cl, cr, ct, cb = info["conf"]
    W, H = info["w"], info["h"]
    left = cl + x
    top = ct + y
    right = cr + (W - x - w)
    bottom = cb + (H - y - h)
    cmd = [ff, "-y", "-hide_banner", "-loglevel", "error",
           "-progress", "pipe:1", "-nostats", "-i", src,
           "-map", "0:v:0", "-map", "0:a?", "-c", "copy"]
    if (left, right, top, bottom) != (0, 0, 0, 0):
        cmd += ["-bsf:v", f"{info['codec']}_metadata="
                         f"crop_left={left}:crop_right={right}"
                         f":crop_top={top}:crop_bottom={bottom}"]
    cmd += ["-movflags", "+faststart", dst]
    return cmd


def build_encode_cmd(ff, src, dst, sel, lossless, crf, preset):
    """模式2/3：crop 滤镜重编码。"""
    x, y, w, h = sel
    cmd = [ff, "-y", "-hide_banner", "-loglevel", "error",
           "-progress", "pipe:1", "-nostats", "-i", src,
           "-map", "0:v:0", "-map", "0:a?",
           "-vf", f"crop={w}:{h}:{x}:{y}",
           "-c:v", "libx265", "-preset", preset]
    if lossless:
        cmd += ["-x265-params", "lossless=1"]
    else:
        cmd += ["-crf", str(crf)]
    cmd += ["-tag:v", "hvc1", "-c:a", "copy",
            "-movflags", "+faststart", dst]
    return cmd


# ------------------------------------------------------------------------ 运镜
# Ken Burns：一张静止的画面（照片，或一段机位锁死的素材）靠取景框随时间移动
# 来产生镜头感。框用浮点数表示，最后交 cv2 重画，所以推近的过程是连续的。
KB_EASES = (("smooth", "缓入缓出"), ("linear", "匀速"),
            ("in", "起步慢、越推越快"), ("out", "起步快、越推越慢"))
KB_EASE_DEF = "smooth"


def kb_ease(t, kind=KB_EASE_DEF):
    """把 0~1 的时间改成 0~1 的进度：缓动改的就是这条坡。"""
    t = clamp(float(t), 0.0, 1.0)
    if kind == "linear":
        return t
    if kind == "in":
        return t ** 3
    if kind == "out":
        return 1.0 - (1.0 - t) ** 3
    return t * t * (3.0 - 2.0 * t)


def kb_rect(a, b, t):
    """两个取景框之间取第 t 个。两端同比例时结果也同比例（分子分母走同一条坡）。"""
    return tuple((1.0 - t) * u + t * v for u, v in zip(a, b))


def kb_rects(a, b, n, kind=KB_EASE_DEF):
    """n 帧的取景框。只有一帧就停在起点，别让单帧片子跑出一条算错的坡。"""
    if n <= 1:
        return [tuple(float(v) for v in a)]
    return [kb_rect(a, b, kb_ease(i / (n - 1.0), kind)) for i in range(n)]


def kb_zoom(rect, out_w, out_h):
    """这一帧被放大几倍。>1 说明框已经比输出画幅还小，再推就是拿少数像素撑大画面。"""
    _x, _y, w, h = rect
    return max(out_w / float(w), out_h / float(h))


def kb_size(rect, long_side):
    """按取景框自己的比例定成片尺寸（长边给足，宽高取偶）。

    运镜不另选比例：框在画面上就是按某个比例锁着拉的，成片跟着它走，
    否则每一帧都要在重画时再裁一刀，用户看到的框就不是最后留下的画面。
    """
    _x, _y, w, h = rect
    if w <= 0 or h <= 0:
        raise ValueError(f"取景框尺寸不合法: {rect}")
    long_side = max(16, int(round(long_side)))
    if w >= h:
        return long_side, max(2, even(long_side * h / float(w)))
    return max(2, even(long_side * w / float(h))), long_side


def kb_refit(a, b, W, H, aspect):
    """换比例时重铺两个取景框，保持"终点在起点里的相对位置与大小"。

    直接对两个框各调一次 refit_rect 会把它们同时夹成画面里的那个最大框：
    运镜幅度就这样悄悄归零了，得按起点框的变化把终点框等比例带过去。
    """
    a2 = refit_rect(a, W, H, aspect)
    if b is None or a[2] <= 0 or a[3] <= 0:
        return a2, a2
    fx, fy = (b[0] - a[0]) / a[2], (b[1] - a[1]) / a[3]
    fw, fh = b[2] / a[2], b[3] / a[3]
    x0, y0 = a2[0] + fx * a2[2], a2[1] + fy * a2[3]
    return a2, norm_rect(x0, y0, x0 + fw * a2[2], y0 + fh * a2[3], W, H)


def kb_frame(img, rect, out_w, out_h):
    """按取景框从 img 取一帧，重画成 out_w×out_h。

    推近（框小于输出画幅）走 warpAffine：框是浮点的，每帧位移都落在亚像素上，
    缓推才不会一帧一帧地跳。缩小改用整数裁切 + INTER_AREA：warpAffine 没有
    面积加权，直接线性采样会在细密纹理上起摩尔纹。
    """
    x, y, w, h = rect
    if w <= 0 or h <= 0:
        raise ValueError(f"取景框尺寸不合法: {rect}")
    out_w, out_h = int(out_w), int(out_h)
    if min(out_w / w, out_h / h) >= 1.0:
        sx, sy = out_w / w, out_h / h
        m = np.float32([[sx, 0.0, -x * sx], [0.0, sy, -y * sy]])
        return cv2.warpAffine(img, m, (out_w, out_h), flags=cv2.INTER_LINEAR,
                              borderMode=cv2.BORDER_REPLICATE)
    fh, fw = img.shape[:2]
    # 取整后可能顶出画面外一两像素（框本身是浮点的），夹回来而不是让它越界
    X = clamp(int(round(x)), 0, max(0, fw - 1))
    Y = clamp(int(round(y)), 0, max(0, fh - 1))
    W = clamp(int(round(w)), 1, fw - X)
    H = clamp(int(round(h)), 1, fh - Y)
    return cv2.resize(img[Y:Y + H, X:X + W], (out_w, out_h),
                      interpolation=cv2.INTER_AREA)

