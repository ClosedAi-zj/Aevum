"""用 littleCMS（Pillow 自带）验收 lapse-core 生成的三份 ICC 配置。

ICC 字节是自己写的，自己测自己不算证据。这个脚本只做一件事：把
`ColorSpace::icc()` 落盘的三份配置交给第三方色彩引擎读，看它读出来的东西
跟我们要的是不是一回事。

先跑一次带环境变量的测试生成配置：

    LAPSE_ICC_DUMP=tests/_tmp_icc cargo test -p lapse-core --lib colorspace

再跑本脚本：

    python acceptance/oracle/check_icc.py [配置目录]

全部通过退出码 0，任何一条不符退出码 1。

注意：本脚本自改名前就没再维护过——`LAPSE_ICC_DUMP` 与 `lapse-core` 都已不存在
（现为 `AEVUM_ICC_DUMP`、`aevum-core`），照现在这样跑不起来。留作历史记录，
不作为验收工具。
"""

from __future__ import annotations

import io
import struct
import sys
from pathlib import Path

import numpy as np
from PIL import Image
from PIL import ImageCms as C

BRADFORD = np.array(
    [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0634],
    ]
)
D65_XY = (0.3127, 0.3290)
D50_XYZ = np.array([0.9642, 1.0, 0.8249])
PRIMARIES = {
    "srgb": [(0.6400, 0.3300), (0.3000, 0.6000), (0.1500, 0.0600)],
    "p3": [(0.6800, 0.3200), (0.2650, 0.6900), (0.1500, 0.0600)],
    "rec2020": [(0.7080, 0.2950), (0.1700, 0.7970), (0.1310, 0.0460)],
}
GAMMA = {"srgb": None, "p3": None, "rec2020": 2.4}
NAMES = {
    "srgb": "Lapse Studio sRGB",
    "p3": "Lapse Studio Display P3",
    "rec2020": "Lapse Studio Rec.2020 (SDR)",
}

FAILS: list[str] = []


def check(ok: bool, label: str, got=None, want=None) -> None:
    if ok:
        print(f"  ok   {label}")
        return
    FAILS.append(label)
    print(f"  FAIL {label}" + ("" if want is None else f": 读到 {got!r}，期望 {want!r}"))


def xy_to_xyz(xy: tuple[float, float]) -> np.ndarray:
    x, y = xy
    return np.array([x / y, 1.0, (1.0 - x - y) / y])


def rgb_to_xyz(primaries) -> np.ndarray:
    """列是原色的 XYZ，缩放让三列之和等于 D65 白点（和 Rust 侧同一套算法）。"""
    a = np.array(
        [
            [p[0] / p[1] for p in primaries],
            [1.0, 1.0, 1.0],
            [(1.0 - p[0] - p[1]) / p[1] for p in primaries],
        ]
    )
    scale = np.linalg.solve(a, xy_to_xyz(D65_XY))
    return a * scale


def chad() -> np.ndarray:
    to = BRADFORD @ D50_XYZ
    from_ = BRADFORD @ xy_to_xyz(D65_XY)
    return np.linalg.inv(BRADFORD) @ np.diag(to / from_) @ BRADFORD


def tag(profile: bytes, sig: bytes) -> bytes:
    count = struct.unpack_from(">I", profile, 128)[0]
    for i in range(count):
        s, off, size = struct.unpack_from(">4sII", profile, 132 + 12 * i)
        if s == sig:
            return profile[off : off + size]
    raise KeyError(sig)


def read_xyz(body: bytes) -> np.ndarray:
    assert body[:4] == b"XYZ ", body[:4]
    return np.array(
        [struct.unpack_from(">i", body, 8 + 4 * i)[0] / 65536.0 for i in range(3)]
    )


def read_sf32(body: bytes) -> np.ndarray:
    assert body[:4] == b"sf32", body[:4]
    v = [struct.unpack_from(">i", body, 8 + 4 * i)[0] / 65536.0 for i in range(9)]
    return np.array(v).reshape(3, 3)


def trc_params(body: bytes) -> np.ndarray:
    assert body[:4] == b"para", body[:4]
    n = (len(body) - 12) // 4
    return np.array([struct.unpack_from(">i", body, 12 + 4 * i)[0] / 65536.0 for i in range(n)])


def ramp(step: int = 32) -> Image.Image:
    g = np.arange(0, 256, step, dtype=np.uint8)
    return Image.fromarray(np.stack([g, g, g], 1).reshape(1, -1, 3), "RGB")


def convert(src: C.ImageCmsProfile, dst: C.ImageCmsProfile, im: Image.Image) -> np.ndarray:
    t = C.buildTransform(src, dst, "RGB", "RGB", C.Intent.ABSOLUTE_COLORIMETRIC)
    return np.array(C.applyTransform(im, t)).reshape(-1, 3)


def srgb_decode(device: np.ndarray) -> np.ndarray:
    """ICC 里 TRC 的语义是 EOTF：设备值 → 线性光。"""
    return np.where(
        device <= 0.04045, device / 12.92, np.power((device + 0.055) / 1.055, 2.4)
    )


def srgb_encode(lin: np.ndarray) -> np.ndarray:
    enc = np.where(lin <= 0.0031308, lin * 12.92, 1.055 * np.power(lin, 1 / 2.4) - 0.055)
    return np.clip(np.round(enc * 255), 0, 255).astype(int)


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).parent.parent / "tests/_tmp_icc")
    profiles = {}
    for key in ("srgb", "p3", "rec2020"):
        path = root / f"{key}.icc"
        if not path.exists():
            print(f"缺少 {path}\n先跑：LAPSE_ICC_DUMP=tests/_tmp_icc cargo test -p lapse-core --lib colorspace")
            return 1
        profiles[key] = path.read_bytes()

    ref = C.createProfile("sRGB")

    for key, blob in profiles.items():
        print(f"[{key}]")
        p = C.getOpenProfile(io.BytesIO(blob))
        check(
            C.getProfileDescription(p).strip() == NAMES[key],
            "littleCMS 读出的描述与写入一致",
            C.getProfileDescription(p),
            NAMES[key],
        )

        # 1. 原色：littleCMS 不参与，这里用独立实现从字节里核 D50 原色与 chad。
        want = chad() @ rgb_to_xyz(PRIMARIES[key])
        for i, sig in enumerate((b"rXYZ", b"gXYZ", b"bXYZ")):
            got = read_xyz(tag(blob, sig))
            check(np.allclose(got, want[:, i], atol=2e-5), f"{sig[:1].decode()}XYZ 对得上独立计算", got, want[:, i])
        check(np.allclose(read_xyz(tag(blob, b"wtpt")), D50_XYZ, atol=2e-5), "wtpt 是 D50 连接空间白点")
        check(np.allclose(read_sf32(tag(blob, b"chad")), chad(), atol=2e-5), "chad 是 Bradford 自适应矩阵")
        colsum = sum(read_xyz(tag(blob, s)) for s in (b"rXYZ", b"gXYZ", b"bXYZ"))
        check(np.allclose(colsum, D50_XYZ, atol=2e-5), "三列原色之和等于白点", colsum, D50_XYZ)

        # 2. 曲线：三条 TRC 必须同一份，且参数就是声明的那条。
        trcs = [tag(blob, s) for s in (b"rTRC", b"gTRC", b"bTRC")]
        check(trcs[0] == trcs[1] == trcs[2], "r/g/bTRC 是同一份曲线")
        par = trc_params(trcs[0])
        if GAMMA[key] is None:
            want_par = np.array([2.4, 1 / 1.055, 0.055 / 1.055, 1 / 12.92, 0.04045, 0.0, 0.0])
        else:
            want_par = np.array([GAMMA[key], 1.0, 0.0, 0.0, 0.0, 0.0, 0.0])
        check(np.allclose(par, want_par, atol=1e-4), "para 参数与声明的曲线一致", par, want_par)

        # 3. 交给 littleCMS：灰阶必须走对，这是像素能不能被正确显示的关键。
        mine = C.getOpenProfile(io.BytesIO(blob))
        got = convert(mine, ref, ramp())[:, 0]
        device = np.arange(0, 256, 32) / 255.0
        lin = srgb_decode(device) if GAMMA[key] is None else np.power(device, GAMMA[key])
        want_ramp = srgb_encode(lin)
        check(np.max(np.abs(got - want_ramp)) <= 1, "littleCMS 把灰阶映射回 sRGB", got, want_ramp)

    # 4. 广色域方向：sRGB 纯红搬到 P3，必须等于公开的矩阵结果（8-bit 下 234,51,35）。
    srgb = C.getOpenProfile(io.BytesIO(profiles["srgb"]))
    p3 = C.getOpenProfile(io.BytesIO(profiles["p3"]))
    red = Image.fromarray(np.array([[[255, 0, 0], [0, 255, 0]]], dtype=np.uint8), "RGB")
    got = convert(srgb, p3, red)
    check(np.allclose(got[0], [234, 51, 35], atol=2), "sRGB 红 → P3 与 littleCMS 一致", got[0], [234, 51, 35])
    lin = np.array([0.822, 0.033, 0.017])
    check(np.allclose(srgb_encode(lin), got[0], atol=2), "同一结果对得上公开矩阵第一列", srgb_encode(lin), got[0])
    check(np.allclose(got[1], [117, 251, 76], atol=2), "sRGB 绿 → P3 与 littleCMS 一致", got[1], [117, 251, 76])

    print()
    if FAILS:
        print(f"共 {len(FAILS)} 条不通过：")
        for f in FAILS:
            print(" -", f)
        return 1
    print("三份配置全部通过 littleCMS 与独立矩阵核算。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
