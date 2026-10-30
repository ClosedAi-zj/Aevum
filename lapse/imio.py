# -*- coding: utf-8 -*-
"""图片读写：中文路径、RAW、16bit。

cv2.imread/imwrite 在 Windows 上不认中文路径（F:\\照片\\… 直接返回 None），
所以统一走 np.fromfile + imdecode / imencode + tofile。
"""
import os
import re

import cv2
import numpy as np

RAW_EXTENSIONS = {'.cr2', '.cr3', '.nef', '.arw', '.raf', '.orf',
                  '.rw2', '.dng', '.pef', '.srw', '.raw', '.3fr', '.rwl',
                  '.x3f', '.mrw'}

IMG_EXTENSIONS = ('.jpg', '.jpeg', '.png', '.tif', '.tiff')


def natural_key(s):
    """PANA2 < PANA10 而不是反过来的排序键。"""
    return [int(t) if t.isdigit() else t.lower()
            for t in re.split(r'(\d+)', s)]


def scan_files(d, exts=IMG_EXTENSIONS, raw=False):
    """列出目录里的图片，按自然序排序。raw=True 时只列 RAW。"""
    want = RAW_EXTENSIONS if raw else {e.lower().lstrip('.') for e in exts}
    out = []
    for name in os.listdir(d):
        p = os.path.join(d, name)
        if not os.path.isfile(p):
            continue
        e = os.path.splitext(name)[1].lower()
        if (raw and e in want) or (not raw and e.lstrip('.') in want):
            out.append(p)
    out.sort(key=lambda p: natural_key(os.path.basename(p)))
    return out


def is_raw(path):
    return os.path.splitext(path)[1].lower() in RAW_EXTENSIONS


def cv_imread(path):
    """8bit BGR，读不到返回 None。"""
    try:
        data = np.fromfile(path, dtype=np.uint8)
        return cv2.imdecode(data, cv2.IMREAD_COLOR)
    except Exception:
        return None


def decode_image(path, bps=8, half_size=False):
    """统一解码入口：RAW 走 rawpy，其它走 OpenCV。返回 BGR uint8/uint16。

    IMREAD_COLOR 会自动应用 EXIF 方向，IMREAD_UNCHANGED 不会；16bit 路径
    用后者，所以这里要自己转一次，否则导出图会横竖颠倒。
    """
    if is_raw(path):
        return decode_raw(path, bps, half_size)
    data = np.fromfile(path, dtype=np.uint8)
    if bps == 16:
        img = cv2.imdecode(data, cv2.IMREAD_UNCHANGED)
        if img is not None:
            img = apply_orientation(img, exif_orientation(path))
    else:
        img = cv2.imdecode(data, cv2.IMREAD_COLOR)
    if img is None:
        return None
    if img.ndim == 2:
        img = cv2.cvtColor(img, cv2.COLOR_GRAY2BGR)
    elif img.shape[2] == 4:
        img = cv2.cvtColor(img, cv2.COLOR_BGRA2BGR)
    return img


def exif_orientation(path):
    """读 JPEG/TIFF 的 EXIF Orientation（1~8）；读不到返回 1。"""
    try:
        with open(path, "rb") as f:
            head = f.read(12)
            if head[:2] in (b"II", b"MM"):
                f.seek(0)
                return _tiff_orientation(f.read(262144))
            if head[:2] != b"\xff\xd8":
                return 1
            f.seek(2)
            for _ in range(128):
                mark = f.read(2)
                if len(mark) < 2 or mark[0] != 0xFF:
                    return 1
                kind = mark[1]
                if kind == 0xDA:
                    return 1
                if kind in (0xD8, 0x01) or 0xD0 <= kind <= 0xD7:
                    continue
                raw = f.read(2)
                if len(raw) < 2:
                    return 1
                size = int.from_bytes(raw, "big")
                if size < 2:
                    return 1
                if kind == 0xE1:
                    seg = f.read(size - 2)
                    if seg.startswith(b"Exif\x00\x00"):
                        return _tiff_orientation(seg[6:])
                else:
                    f.seek(size - 2, 1)
    except (OSError, ValueError):
        pass
    return 1


# EXIF 标签号：ISO 速度、曝光时间、光圈 F 值，以及它们缺失时的 APEX 备用值
_TAG_ISO, _TAG_EXP, _TAG_FNUM = 0x8827, 0x829A, 0x829D
_TAG_TV, _TAG_AV = 0x9201, 0x9202
_TAG_EXIF_IFD = 0x8769


def _tag_value(data, order, typ, num, e):
    """读一个 IFD 条目的首个值；只认 BYTE/SHORT/LONG/RATIONAL/SRATIONAL。"""
    if num < 1:
        return None

    def u16(o):
        return int.from_bytes(data[o:o + 2], order) \
            if 0 <= o and o + 2 <= len(data) else None

    def u32(o):
        return int.from_bytes(data[o:o + 4], order) \
            if 0 <= o and o + 4 <= len(data) else None

    unit = {1: 1, 3: 2, 4: 4, 5: 8, 10: 8}.get(typ)
    if unit is None:
        return None
    inline = unit * num <= 4
    off = e + 8 if inline else u32(e + 8)
    if off is None:
        return None
    if typ == 1:
        return float(data[off]) if 0 <= off < len(data) else None
    if typ == 3:
        v = u16(off)
        return float(v) if v is not None else None
    if typ == 4:
        v = u32(off)
        return float(v) if v is not None else None
    nu = u32(off)
    de = u32(off + 4)
    if nu is None or not de:
        return None
    v = nu / de
    if typ == 10 and nu >= 0x80000000:
        v = (nu - 0x100000000) / de
    return float(v)


def _tiff_tags(data, want):
    """扫 TIFF 的 IFD0 与 Exif 子 IFD，返回 {tag: float}。

    JPEG 的 ISO/快门/光圈基本都在 Exif 子 IFD，只扫 IFD0 读不到；RAW 的
    排布更乱，能读到多少算多少，读不到由调用方退回亮度推断。
    """
    if len(data) < 8 or data[:2] not in (b"II", b"MM"):
        return {}
    order = "little" if data[:2] == b"II" else "big"
    if int.from_bytes(data[2:4], order) != 42:
        return {}
    want = {int(t) for t in want}
    out = {}
    queue = [int.from_bytes(data[4:8], order)]
    seen = set()
    while queue and len(seen) < 8:
        off = queue.pop(0)
        if off in seen or off < 8 or off + 2 > len(data):
            continue
        seen.add(off)
        count = int.from_bytes(data[off:off + 2], order)
        for i in range(min(count, 4096)):
            e = off + 2 + i * 12
            if e + 12 > len(data):
                break
            tag = int.from_bytes(data[e:e + 2], order)
            if tag == _TAG_EXIF_IFD:
                ptr = int.from_bytes(data[e + 8:e + 12], order)
                if ptr not in seen:
                    queue.append(ptr)
                continue
            if tag not in want:
                continue
            typ = int.from_bytes(data[e + 2:e + 4], order)
            num = int.from_bytes(data[e + 4:e + 8], order)
            # 有些写入器把曝光时间/光圈错写成两个 SHORT（分数对），当 RATIONAL 读
            if typ == 3 and num == 2 and tag in (_TAG_EXP, _TAG_FNUM):
                a = int.from_bytes(data[e + 8:e + 10], order)
                b = int.from_bytes(data[e + 10:e + 12], order)
                if b:
                    out[tag] = a / b
                continue
            v = _tag_value(data, order, typ, num, e)
            if v is not None:
                out[tag] = v
    return out


def exif_tags(path, want):
    """读 JPEG/TIFF/RAW 的指定 EXIF 标签；读不到返回 {}。"""
    try:
        want = tuple(want)
        with open(path, "rb") as f:
            head = f.read(12)
            if head[:2] in (b"II", b"MM"):
                f.seek(0)
                return _tiff_tags(f.read(1048576), want)
            if head[:2] != b"\xff\xd8":
                return {}
            f.seek(2)
            for _ in range(128):
                mark = f.read(2)
                if len(mark) < 2 or mark[0] != 0xFF:
                    return {}
                kind = mark[1]
                if kind == 0xDA:
                    return {}
                if kind in (0xD8, 0x01) or 0xD0 <= kind <= 0xD7:
                    continue
                raw = f.read(2)
                if len(raw) < 2:
                    return {}
                size = int.from_bytes(raw, "big")
                if size < 2:
                    return {}
                if kind == 0xE1:
                    seg = f.read(size - 2)
                    if seg.startswith(b"Exif\x00\x00"):
                        return _tiff_tags(seg[6:], want)
                else:
                    f.seek(size - 2, 1)
    except (OSError, ValueError):
        pass
    return {}


def exif_exposure(path):
    """读一帧的曝光三要素与相对档数 EV，供 Holy Grail 台阶检测。

    EV 以 ISO100/1 秒/f1.0 为 0 点（只做跨帧比较，绝对值没有意义）；
    宁可返回 None 也不猜：读不全会让台阶检测误判。
    """
    t = exif_tags(path, (_TAG_ISO, _TAG_EXP, _TAG_FNUM, _TAG_TV, _TAG_AV))
    iso = t.get(_TAG_ISO)
    exp = t.get(_TAG_EXP)
    if not exp and _TAG_TV in t:
        try:
            exp = 2.0 ** (-t[_TAG_TV])
        except OverflowError:
            exp = None
    fnum = t.get(_TAG_FNUM)
    if not fnum and _TAG_AV in t:
        fnum = 2.0 ** (t[_TAG_AV] / 2.0)
    ev = None
    if iso and exp and fnum and iso > 0 and exp > 0 and fnum > 0:
        import math
        ev = (math.log2(iso / 100.0) - math.log2(exp)
              + 2.0 * math.log2(fnum))
    return {"ev": ev, "iso": iso, "exp": exp, "f": fnum}


def exif_ev(path):
    """EV 标量版 exif_exposure；None 表示读不到。"""
    return exif_exposure(path)["ev"]


def _tiff_orientation(data):
    if len(data) < 8:
        return 1
    order = "little" if data[:2] == b"II" else "big"
    if int.from_bytes(data[2:4], order) != 42:
        return 1
    off = int.from_bytes(data[4:8], order)
    if off + 2 > len(data):
        return 1
    count = int.from_bytes(data[off:off + 2], order)
    for i in range(min(count, 4096)):
        e = off + 2 + i * 12
        if e + 12 > len(data):
            break
        if int.from_bytes(data[e:e + 2], order) != 0x0112:
            continue
        typ = int.from_bytes(data[e + 2:e + 4], order)
        num = int.from_bytes(data[e + 4:e + 8], order)
        if typ == 3 and num >= 1:
            v = int.from_bytes(data[e + 8:e + 10], order)
            return v if 1 <= v <= 8 else 1
        return 1
    return 1


def apply_orientation(img, orientation):
    """按 EXIF Orientation 旋转/镜像数组（2~8；1 原样）。"""
    o = int(orientation or 1)
    if o == 2:
        return img[:, ::-1].copy()
    if o == 3:
        return img[::-1, ::-1].copy()
    if o == 4:
        return img[::-1].copy()
    if o == 5:
        return cv2.transpose(img)
    if o == 6:
        return cv2.rotate(img, cv2.ROTATE_90_CLOCKWISE)
    if o == 7:
        return cv2.flip(cv2.transpose(img), -1)
    if o == 8:
        return cv2.rotate(img, cv2.ROTATE_90_COUNTERCLOCKWISE)
    return img


def decode_raw(path, bps=8, half_size=False):
    try:
        import rawpy
    except ImportError:
        raise RuntimeError(
            "处理 RAW 需要 rawpy：pip install rawpy "
            "（慢的话加 -i https://mirrors.aliyun.com/pypi/simple/）")
    with rawpy.imread(path) as raw:
        rgb = raw.postprocess(
            use_camera_wb=True,
            output_bps=bps,
            half_size=half_size,
            # 固定亮度曲线：rawpy 默认会按每帧自动增亮，延时序列里曝光
            # 本来就有起伏，逐帧自适应会把这种起伏放大成闪烁。
            no_auto_bright=True,
            gamma=(2.222, 4.5),
            demosaic_algorithm=rawpy.DemosaicAlgorithm.AHD,
            output_color=rawpy.ColorSpace.sRGB,
        )
    return cv2.cvtColor(rgb, cv2.COLOR_RGB2BGR)


def image_size(path):
    """只读宽高，失败返回 (0, 0)。"""
    if is_raw(path):
        try:
            import rawpy
            with rawpy.imread(path) as raw:
                w, h = int(raw.sizes.width), int(raw.sizes.height)
                # postprocess 默认会按 RAW 方向转正，sizes 仍是传感器方向；
                # 90° 旋转的竖拍要交换宽高，否则对齐/裁剪比例会错。
                if int(getattr(raw.sizes, "flip", 0) or 0) in (5, 6):
                    w, h = h, w
                return (w, h)
        except Exception:
            return (0, 0)
    try:
        im = cv2.imdecode(np.fromfile(path, dtype=np.uint8),
                          cv2.IMREAD_GRAYSCALE)
        return (im.shape[1], im.shape[0]) if im is not None else (0, 0)
    except Exception:
        return (0, 0)


def cv_imwrite(path, img, quality=95):
    """按扩展名编码并写盘；16bit 只在 png/tif 里保留，jpg 会降为 8bit。"""
    ext = os.path.splitext(path)[1].lower() or '.jpg'
    try:
        if ext in ('.jpg', '.jpeg'):
            if img.dtype == np.uint16:
                img = (img >> 8).astype(np.uint8)
            ok, buf = cv2.imencode('.jpg', img,
                                   [cv2.IMWRITE_JPEG_QUALITY, quality])
        elif ext == '.png':
            ok, buf = cv2.imencode('.png', img, [cv2.IMWRITE_PNG_COMPRESSION, 3])
        elif ext in ('.tif', '.tiff'):
            ok, buf = cv2.imencode('.tif', img)
        else:
            ok, buf = cv2.imencode(ext, img)
        if not ok:
            return False
        buf.tofile(path)
        return True
    except Exception:
        return False
