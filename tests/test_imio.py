# -*- coding: utf-8 -*-
"""图像 IO 回归：16bit 读写、EXIF 方向、中文路径。

8bit 源走 OpenCV 自动转正；16bit/RAW 走 IMREAD_UNCHANGED，必须自己按
Orientation 转一次。这里用 Pillow 的 exif_transpose 当方向参照。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_imio.py
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "acceptance"))
TMP = os.path.join(HERE, "_tmp", "imio")
os.makedirs(TMP, exist_ok=True)

import cv2                                # noqa: E402
import numpy as np                        # noqa: E402

from reference import imio                    # noqa: E402
from reference.gui.grader import exporter     # noqa: E402

FAILS = []


def check(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}"
          + (f"  {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def test_orientation_map():
    try:
        from PIL import Image, ImageOps
    except ImportError:
        print("[SKIP] 没有 Pillow，跳过方向参照")
        return
    base = (np.arange(4 * 6 * 3, dtype=np.uint8).reshape(4, 6, 3) * 3)
    for o in range(1, 9):
        im = Image.fromarray(base)
        im.getexif()[0x0112] = o
        ref = np.array(ImageOps.exif_transpose(im))
        got = imio.apply_orientation(base, o)
        check(f"方向 {o} 与 Pillow 一致",
              got.shape == ref.shape and np.array_equal(got, ref),
              f"{got.shape} vs {ref.shape}")


def _exif_jpeg(path, orientation):
    from PIL import Image
    h, w = 48, 72
    x = np.linspace(0, 255, w, dtype=np.uint8)
    y = np.linspace(0, 255, h, dtype=np.uint8)
    arr = np.stack([np.tile(x, (h, 1)), np.tile(y[:, None], (1, w)),
                    np.full((h, w), 90, np.uint8)], axis=2)
    im = Image.fromarray(arr)
    ex = im.getexif()
    ex[0x0112] = orientation
    im.save(path, quality=95, exif=ex)
    return arr


def test_exif_jpeg():
    try:
        from PIL import ImageOps
    except ImportError:
        return
    p = os.path.join(TMP, "定向 6.jpg")          # 顺带测中文/空格路径
    _exif_jpeg(p, 6)
    check("EXIF Orientation 解析", imio.exif_orientation(p) == 6,
          str(imio.exif_orientation(p)))
    from PIL import Image
    with Image.open(p) as im:
        ref = np.array(ImageOps.exif_transpose(im))
    a = imio.decode_image(p, bps=8)
    b = imio.decode_image(p, bps=16)
    ok_shape = (a is not None and b is not None
                and a.shape[:2] == ref.shape[:2] == b.shape[:2]
                and a.shape[1] == 48)            # 6 = 顺时针 90°，宽高交换
    check("8bit/16bit 解码都按 EXIF 转正", ok_shape,
          f"{None if a is None else a.shape} / "
          f"{None if b is None else b.shape} / 参照 {ref.shape}")
    if ok_shape:
        diff = float(np.abs(a[:, :, ::-1].astype(np.float32)
                            - ref.astype(np.float32)).mean())
        check("转正后的像素方向正确", diff < 8, f"平均差 {diff:.1f}")

    torn = os.path.join(TMP, "truncated.jpg")
    with open(p, "rb") as f:
        data = f.read(400)
    with open(torn, "wb") as f:
        f.write(data)
    check("坏 EXIF 不崩", imio.exif_orientation(torn) in range(1, 9))


def test_16bit_roundtrip():
    h, w = 40, 64
    ramp = np.linspace(0, 1, w, dtype=np.float32)
    arr = np.stack([np.tile(ramp, (h, 1)),
                    np.tile(ramp[::-1], (h, 1)),
                    np.full((h, w), 0.37, np.float32)], axis=2)
    cases = (("16bit.png", 16, np.uint16), ("16bit.tif", 16, np.uint16),
             ("8bit.png", 8, np.uint8), ("fallback.jpg", 16, np.uint8))
    for name, bit, dtype in cases:
        p = os.path.join(TMP, name)
        ok = exporter.save_image(p, arr, bit=bit)
        dec = imio.decode_image(p, bps=16)
        check(f"{name} 写盘并解码", ok and dec is not None
              and dec.dtype == dtype, "" if dec is None else str(dec.dtype))
        if dec is None:
            continue
        back = cv2.cvtColor(dec, cv2.COLOR_BGR2RGB).astype(np.float32)
        back /= 65535.0 if dec.dtype == np.uint16 else 255.0
        tol = 2.0 / (65535.0 if dec.dtype == np.uint16 else 255.0)
        if name.endswith(".jpg"):               # 有损压缩按次采样误差放宽
            tol = 16.0 / 255.0
        check(f"{name} 数值可还原", float(np.abs(back - arr).max()) <= tol,
              f"最大差 {float(np.abs(back - arr).max()):.2e}")


def test_batch_export():
    from PySide6 import QtCore
    from reference.gui.grader import developer

    QtCore.QCoreApplication.instance() or QtCore.QCoreApplication([])
    src = []
    for i in range(3):
        p = os.path.join(TMP, f"src_{i}.jpg")
        im = np.full((32, 48, 3), 60 + i * 40, np.uint8)
        cv2.imwrite(p, im)
        src.append(p)
    out = os.path.join(TMP, "batch")
    os.makedirs(out, exist_ok=True)
    for f in os.listdir(out):
        os.remove(os.path.join(out, f))

    got = {}
    job = developer.BatchExport(src, out, developer.gc.defaults(),
                                fmt="PNG", bit=16)
    job.done.connect(lambda ok, res: got.update(ok=ok, res=res))
    job.run()
    files = sorted(os.listdir(out))
    dtype = imio.decode_image(os.path.join(out, files[0]), bps=16).dtype \
        if files else None
    check("批量 PNG 16bit 扩展名与位深", got.get("ok")
          and files == ["src_0.png", "src_1.png", "src_2.png"]
          and dtype == np.uint16, f"{files} {dtype}")

    got.clear()
    job = developer.BatchExport(src + [os.path.join(TMP, "不存在.jpg")],
                                out, developer.gc.defaults(), fmt="TIFF",
                                bit=16)
    job.done.connect(lambda ok, res: got.update(ok=ok, res=res))
    job.run()
    check("坏图不拖死整批", not got.get("ok") and len(got.get("res") or []) == 3,
          f"成功 {len(got.get('res') or [])} 张")

    got.clear()
    seen = {}

    def per_frame(path):
        seen[os.path.basename(path)] = True
        return {**developer.gc.defaults(),
                "exposure": 1.0 if path.endswith("src_2.jpg") else 0.0}

    job = developer.BatchExport(src, out, developer.gc.defaults(), fmt="JPG",
                                bit=16, params_at=per_frame)
    job.done.connect(lambda ok, res: got.update(ok=ok, res=res))
    job.run()
    a = imio.decode_image(os.path.join(out, "src_0.jpg"), bps=8)
    b = imio.decode_image(os.path.join(out, "src_2.jpg"), bps=8)
    check("逐帧参数与 JPG 8bit 回退", got.get("ok") and len(seen) == 3
          and a is not None and b is not None
          and float(b.mean()) > float(a.mean()) * 1.6,
          f"{float(a.mean()):.0f} -> {float(b.mean()):.0f}")


def main():
    test_orientation_map()
    test_exif_jpeg()
    test_16bit_roundtrip()
    test_batch_export()
    print()
    if FAILS:
        print("失败 " + str(len(FAILS)) + ": " + str(FAILS))
        sys.exit(1)
    print("全部通过")


if __name__ == "__main__":
    main()
