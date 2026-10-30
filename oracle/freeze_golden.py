# -*- coding: utf-8 -*-
"""冻结当前 Python 实现的数值结果，作为 Rust 重写的回归标尺。

只读素材、只写 oracle/golden/*.json。跑两遍结果必须逐字节一致，否则说明
基准本身不稳定（进程池顺序、JPEG 解码差异），不能拿来比对。

用法（仓库根目录，用开发树的解释器）：
    python oracle/freeze_golden.py --frames ../pythonProject4/aligned_lr3 --count 24
"""
import argparse
import hashlib
import json
import platform
import sys
from pathlib import Path

import cv2
import numpy as np

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from lapse import align, deflicker, grade, imio, luma as lumamod  # noqa: E402

IMAGE_EXT = (".jpg", ".jpeg", ".png", ".tif", ".tiff")


def rnd(x, digits=6):
    if x is None:
        return None
    v = round(float(x), digits)
    return 0.0 if v == 0 else v


def digest_floats(values):
    arr = np.asarray(values, dtype=np.float32).reshape(-1)
    return hashlib.sha256(arr.tobytes()).hexdigest()


def grade_snapshot():
    curve_pts = [[0, 0], [32, 18], [128, 140], [224, 236], [255, 255]]
    lut = grade.pchip_lut(curve_pts, 256)
    vig_p = {"vig_amount": 40.0, "vig_midpoint": 50.0,
             "vig_roundness": 25.0, "vig_feather": 50.0}
    vig = grade.vignette_mask(64, 40, vig_p)
    samples = []
    if vig is not None:
        for y in range(0, 40, 8):
            row = [rnd(vig[y, x]) for x in range(0, 64, 8)]
            samples.append(row)
    return {
        "curve_points": curve_pts,
        "curve_lut": [rnd(v) for v in lut],
        "vignette": {
            "size": [64, 40],
            "params": vig_p,
            "sha256_f32": digest_floats(vig) if vig is not None else None,
            "samples_8x5": samples,
        },
        "defaults": grade.defaults(),
        "presets": {name: grade.preset(name) for name in sorted(grade.PRESETS)},
        "mask_new": grade.MASK_NEW,
    }


DEV_H, DEV_W = 48, 64


def _dev_image(kind):
    """显影金标用的合成图：只用确定性公式，不许带随机源。

    三张图分别喂给逐点运算、空间运算与降噪/锐化：HSL 与色彩分级要覆盖整圈
    色相，清晰度/锐化要有真实边缘，降噪要真的有噪点。
    """
    y, x = np.mgrid[0:DEV_H, 0:DEV_W]
    y = y.astype(np.float32)
    x = x.astype(np.float32)
    u = x / float(DEV_W - 1)
    v = y / float(DEV_H - 1)
    if kind == "smooth":
        hsv = np.empty((DEV_H, DEV_W, 3), dtype=np.float32)
        hsv[..., 0] = u * 359.0                      # 横向扫完整圈色相
        hsv[..., 1] = 0.20 + 0.75 * v                # 纵向扫饱和度
        hsv[..., 2] = 0.10 + 0.80 * (0.5 + 0.5 * np.sin(u * 6.2831853 * 2))
        rgb = cv2.cvtColor(hsv, cv2.COLOR_HSV2RGB)
    elif kind == "structure":
        step = ((x > 18) & (y < 30)).astype(np.float32) * 0.55
        diag = np.abs(u - v).astype(np.float32)
        dx = (x - DEV_W / 2) / float(DEV_W)
        dy = (y - DEV_H / 2) / float(DEV_H)
        disk = np.clip(1.0 - (dx * dx + dy * dy) * 9.0, 0.0, 1.0).astype(np.float32)
        stripes = (0.5 + 0.5 * np.sin(x * 1.1)).astype(np.float32) * 0.35
        rgb = np.stack([
            np.clip(0.10 + step + 0.55 * diag, 0.0, 1.0),
            np.clip(0.08 + 0.75 * disk + stripes, 0.0, 1.0),
            np.clip(0.12 + step * 0.6 + stripes + 0.3 * disk, 0.0, 1.0),
        ], axis=-1)
    else:  # "noisy"：中灰底 + 三通道互不相关的可复现伪随机噪点 + 几条硬边
        idx = np.arange(DEV_H * DEV_W, dtype=np.uint64).reshape(DEV_H, DEV_W)
        fields = []
        for ch, mult in enumerate((0x9E3779B97F4A7C15, 0xBF58476D1CE4E5B9,
                                   0x94D049BB133111EB)):
            s = (idx * mult) + np.uint64(0xD1B54A32D192ED03 + ch * 0x1000)
            s ^= s >> 30
            s *= np.uint64(0xBF58476D1CE4E5B9)
            s ^= s >> 27
            s *= np.uint64(0x94D049BB133111EB)
            s ^= s >> 31
            fields.append(s.astype(np.float32) / np.float32(2 ** 64) - 0.5)
        edge = ((x > 12) & (x < 16)).astype(np.float32) + ((y > 20) & (y < 24)).astype(np.float32)
        base = 0.42 + 0.10 * u - 0.06 * v + 0.30 * edge
        rgb = np.stack([
            np.clip(base + fields[0] * 0.22, 0.0, 1.0),
            np.clip(base + fields[1] * 0.20 + 0.05, 0.0, 1.0),
            np.clip(base + fields[2] * 0.24 - 0.02, 0.0, 1.0),
        ], axis=-1)
    return np.ascontiguousarray(np.clip(rgb, 0.0, 1.0), dtype=np.float32)


def _dev_hex(arr):
    return np.ascontiguousarray(arr, dtype=np.float32).tobytes().hex()


def _dev_params(**over):
    p = grade.defaults()
    p.update(over)
    return p


def _dev_stats(img):
    flat = img.reshape(-1, 3).astype(np.float64)
    return [
        {"mean": rnd(flat[:, c].mean()), "std": rnd(flat[:, c].std()),
         "p01": rnd(np.percentile(flat[:, c], 1)),
         "p99": rnd(np.percentile(flat[:, c], 99))}
        for c in range(3)
    ]


# 参数键 → 它所属的 Rust 实现阶段。Rust 侧 `develop::STAGES_READY` 没列全的
# 阶段，对应的 case 会被跳过；列全了就必须逐像素对上。键表由参数自动推，
# 不手写，免得出新 case 时忘了标、把没实现的阶段混进比对着。
_DEV_STAGE_OF = {
    "exposure": "tone", "contrast": "tone", "highlights": "tone",
    "shadows": "tone", "whites": "tone", "blacks": "tone",
    "temperature": "tone", "tint": "tone", "vibrance": "tone",
    "chan_ev": "tone", "saturation": "tone", "dehaze": "tone", "curve_rgb": "tone",
    "hl_rolloff": "tone", "black_soft": "tone", "white_soft": "tone",
    "curve_hdr_tail": "tone", "contrast_linear": "tone",
    "lv_black": "lut", "lv_white": "lut", "lv_gamma": "lut",
    "lv_out_black": "lut", "lv_out_white": "lut",
    "par_shadows": "lut", "par_darks": "lut", "par_lights": "lut",
    "par_highlights": "lut", "par_split1": "lut", "par_split2": "lut",
    "par_split3": "lut",
    "curve_r": "lut", "curve_g": "lut", "curve_b": "lut",
    "hsl_h": "lut", "hsl_s": "lut", "hsl_l": "lut",
    "mono": "lut", "mono_h": "lut",
    "clarity": "spatial", "texture": "spatial",
    "sharp_amount": "spatial", "sharp_radius": "spatial",
    "sharp_detail": "spatial", "sharp_masking": "spatial",
    "sharp_protect": "spatial",
    "nr_luma": "spatial", "nr_color": "spatial", "nr_luma_detail": "spatial",
    "nr_luma_contrast": "spatial", "nr_color_smooth": "spatial",
    "nr_multiscale": "spatial", "nr_sigma_manual": "spatial",
    "dehaze_dark": "spatial", "dehaze_contrast": "spatial",
    "lens_distort": "spatial", "lens_vignette": "spatial", "lens_fringe": "spatial",
    "vignette": "vignette", "vig_midpoint": "vignette", "vig_roundness": "vignette",
    "vig_feather": "vignette", "vig_hl_contrast": "vignette",
    "grain": "grain", "grain_size": "grain", "grain_freq": "grain",
    "grain_lum": "grain",
}
_DEV_SPATIAL_KEYS = {k for k, v in _DEV_STAGE_OF.items() if v == "spatial"}


def _dev_stages(params):
    """这个 case 压到了哪些阶段，按参数键自动推，结果稳定可排序。"""
    base = grade.defaults()
    stages = set()
    for key, val in params.items():
        if key == "masks":
            continue
        if key == "grade_balance" or key.startswith("grade_"):
            stage = "lut"
        else:
            stage = _DEV_STAGE_OF.get(key)
        if stage is None:
            raise KeyError(f"参数 {key} 没登记所属阶段")
        if isinstance(val, (int, float)):
            same = val == base.get(key, 0.0)
        else:
            same = val == base.get(key)
        if not same:
            stages.add(stage)
    for m in params.get("masks") or ():
        groups = [m] + list(m.get("components") or ())
        for g in groups:
            for key, val in (g.get("params") or {}).items():
                if key.startswith("grade_"):
                    continue
                stage = _DEV_STAGE_OF.get(key)
                if stage is None:
                    raise KeyError(f"蒙版参数 {key} 没登记所属阶段")
                if val == (grade.defaults().get(key, 0.0) or 0.0):
                    continue
                stages.add("mask_local" if stage == "spatial" else "mask")
        if "components" in m:
            stages.add("mask")
    return sorted(stages)


def cube_snapshot():
    """.cube 导出基准：与 grade.write_cube 完全同一条路（for_lut 的那条链）。

    每个 case 存 17³ 格点的输出（小端 f32 hex）与格点顺序说明；另外把其中一个
    case 的完整文本存下来，Rust 侧既要对数也要对文件头。红分量变化最快是
    .cube 的标准顺序，搞反了色彩会整体错位，所以专门留 identity case 抓它。
    """
    curve = [[0, 0], [48, 30], [128, 150], [200, 214], [255, 255]]
    cases = [
        ("identity", "默认参数：只验格点顺序与定义域", _dev_params()),
        ("tone", "影调 + 白平衡 + 曲线",
         _dev_params(exposure=0.5, contrast=24.0, highlights=-22.0, shadows=20.0,
                     temperature=25.0, tint=-12.0, vibrance=18.0, dehaze=12.0,
                     curve_rgb=curve)),
        ("clip", "强对比推到端点，验 0/1 截断",
         _dev_params(contrast=90.0, blacks=-60.0, whites=60.0, exposure=0.8)),
        ("bw", "单色 + 混合",
         _dev_params(mono=100.0,
                     mono_h=[38.0, -22.0, 14.0, 55.0, -30.0, 20.0, -18.0, 33.0])),
        ("all_point", "全部逐点项叠一起",
         _dev_params(exposure=0.35, contrast=18.0, highlights=-22.0, shadows=18.0,
                     temperature=20.0, tint=-10.0, vibrance=14.0, saturation=-6.0,
                     curve_rgb=curve, lv_black=10.0, lv_white=246.0, lv_gamma=1.15,
                     par_shadows=15.0, par_lights=18.0,
                     hsl_s=[40.0, 0.0, -30.0, 25.0, 0.0, 36.0, 0.0, -20.0],
                     grade_sh_h=20.0, grade_sh_s=30.0, grade_hi_h=40.0,
                     grade_hi_s=25.0, mono=60.0)),
    ]
    out = {
        "layout": "size³ 格点，红分量变化最快（.cube 标准顺序），"
                  "每格小端 float32，hex 编码，取值 [0,1]",
        "size": 17,
        "cases": [],
    }
    text = None
    for name, note, p in cases:
        size = 17
        ax = np.linspace(0.0, 1.0, size, dtype=np.float32)
        lat = np.empty((size, size, size, 3), dtype=np.float32)
        lat[..., 0] = ax.reshape(1, 1, -1)   # r：最快
        lat[..., 1] = ax.reshape(1, -1, 1)   # g：次之
        lat[..., 2] = ax.reshape(-1, 1, 1)   # b：最慢
        res = np.clip(grade.apply(lat, p, for_lut=True), 0.0, 1.0)
        out["cases"].append({
            "name": name,
            "note": note,
            "stage": _dev_stages(p),
            "size": size,
            "params": p,
            "out_hex": _dev_hex(res),
        })
        if name == "tone":
            import io
            buf = io.StringIO()
            buf.write('# ColorTool 3D LUT\nTITLE "grade"\n'
                      f'LUT_3D_SIZE {size}\n'
                      "DOMAIN_MIN 0.0 0.0 0.0\nDOMAIN_MAX 1.0 1.0 1.0\n")
            np.savetxt(buf, res.reshape(-1, 3), fmt="%.6f")
            text = buf.getvalue()
    out["cube_text"] = text
    out["cube_text_case"] = "tone"
    return out


def develop_snapshot():
    """整条显影链的逐像素基准：Rust 的 develop::render 必须逐张对上。

    每个 case 记录用了哪张输入图、参数、期望输出（小端 float32 的 hex，
    行优先 H×W×3）、以及压到了哪些阶段。颗粒是 numpy PCG64 生成的，
    Rust 复现不了同一串随机数，所以 grain case 只存统计量并标 "grain": true。
    """
    curve = [[0, 0], [48, 30], [128, 150], [200, 214], [255, 255]]
    curve_r = [[0, 0], [64, 52], [192, 208], [255, 255]]
    curve_g = [[0, 12], [128, 120], [255, 240]]
    curve_b = [[0, 0], [96, 118], [255, 255]]

    cases = [
        {
            "name": "tone_and_curves",
            "note": "白平衡/曝光/对比/四段影调/去朦胧 + 分通道曲线 + 色阶 + 参数曲线",
            "image": "smooth",
            "params": _dev_params(
                exposure=0.55, contrast=22.0, highlights=-30.0, shadows=26.0,
                whites=12.0, blacks=-14.0, temperature=28.0, tint=-16.0,
                vibrance=18.0, saturation=-9.0, dehaze=15.0,
                curve_rgb=curve, curve_r=curve_r, curve_g=curve_g, curve_b=curve_b,
                lv_black=12.0, lv_white=244.0, lv_gamma=1.18,
                lv_out_black=6.0, lv_out_white=250.0,
                par_shadows=18.0, par_darks=-12.0, par_lights=22.0,
                par_highlights=-16.0, par_split1=22.0, par_split2=52.0,
                par_split3=80.0),
        },
        {
            "name": "ev_per_channel",
            "note": "分通道去闪叠在白平衡上：chan_ev 均值不为 0，"
                    "Rust 侧要能证明“曝光吃均值 + 残差补色差”与 Python 逐通道相加等价",
            "image": "smooth",
            "params": _dev_params(exposure=0.3, temperature=18.0, tint=-8.0,
                                  contrast=14.0, chan_ev=[0.08, 0.02, -0.04]),
        },
        {
            "name": "hsl_and_mono",
            "note": "HSL 8 组三轴 + 单色 + 黑白混合 8 通道（三轴与 mono 都是 ±100 量纲）",
            "image": "smooth",
            "params": _dev_params(
                hsl_h=[30.0, -20.0, 0.0, 45.0, -35.0, 15.0, 0.0, -50.0],
                hsl_s=[60.0, -40.0, 20.0, 0.0, -70.0, 50.0, 0.0, -25.0],
                hsl_l=[0.0, 35.0, -45.0, 12.0, 0.0, -30.0, 55.0, 0.0],
                mono=100.0,
                mono_h=[38.0, -22.0, 14.0, 55.0, -30.0, 20.0, -18.0, 33.0]),
        },
        {
            "name": "grade_wheels",
            "note": "色彩分级：全局轮 + 阴影/中间调/高光 + 均衡 + 混合（S/L 是 ±100 量纲）",
            "image": "smooth",
            "params": _dev_params(
                grade_global_h=210.0, grade_global_s=18.0, grade_global_l=6.0,
                grade_sh_h=18.0, grade_sh_s=35.0, grade_sh_l=-12.0,
                grade_mid_h=170.0, grade_mid_s=15.0, grade_mid_l=4.0,
                grade_hi_h=42.0, grade_hi_s=28.0, grade_hi_l=10.0,
                grade_balance=45.0, grade_blending=60.0),
        },
        {
            "name": "detail_spatial",
            "note": "降噪（最先）+ 锐化 + 清晰度 + 纹理；输入是带噪点的图",
            "image": "noisy",
            "params": _dev_params(
                nr_luma=32.0, nr_color=28.0, nr_luma_detail=15.0,
                nr_luma_contrast=20.0, nr_color_smooth=60.0,
                sharp_amount=65.0, sharp_radius=1.2, sharp_detail=40.0,
                sharp_masking=30.0, clarity=45.0, texture=35.0,
                exposure=0.2, temperature=10.0),
        },
        {
            "name": "lens",
            "note": "镜头校正：畸变 remap + 暗角补偿 + 去紫边",
            "image": "structure",
            "params": _dev_params(
                lens_distort=32.0, lens_vignette=-25.0, lens_fringe=45.0),
        },
        {
            "name": "all_point",
            "note": "全部逐点项叠在一起，专门抓顺序错误",
            "image": "smooth",
            "params": _dev_params(
                exposure=0.4, contrast=18.0, highlights=-25.0, shadows=20.0,
                whites=10.0, blacks=-12.0, temperature=22.0, tint=-12.0,
                vibrance=15.0, saturation=-8.0, dehaze=12.0,
                curve_rgb=curve, curve_r=curve_r, curve_b=curve_b,
                lv_black=10.0, lv_white=246.0, lv_gamma=1.15,
                par_shadows=15.0, par_lights=18.0, par_split2=48.0,
                hsl_h=[25.0, 0.0, -30.0, 45.0, 0.0, 20.0, -15.0, 0.0],
                hsl_s=[40.0, 0.0, -30.0, 25.0, 0.0, 36.0, 0.0, -20.0],
                hsl_l=[0.0, 30.0, -25.0, 0.0, 20.0, 0.0, -28.0, 0.0],
                grade_sh_h=20.0, grade_sh_s=30.0, grade_hi_h=40.0,
                grade_hi_s=25.0, grade_balance=40.0, grade_blending=55.0,
                mono=70.0, mono_h=[25.0, -15.0, 10.0, 30.0, -20.0, 12.0, -8.0, 20.0]),
        },
        {
            "name": "all_full",
            "note": "逐点 + 空间 + 镜头 + 晕影（不含颗粒与蒙版）",
            "image": "structure",
            "params": _dev_params(
                exposure=0.35, contrast=20.0, highlights=-20.0, shadows=18.0,
                temperature=15.0, tint=-8.0, vibrance=12.0, dehaze=10.0,
                clarity=35.0, texture=25.0, sharp_amount=50.0,
                sharp_radius=1.0, sharp_detail=30.0, nr_luma=18.0, nr_color=15.0,
                hsl_s=[0.15, 0.0, -0.1, 0.2, 0.0, 0.0, 0.1, 0.0],
                grade_mid_h=190.0, grade_mid_s=0.12, grade_blending=40.0,
                curve_rgb=curve, lv_gamma=1.1, par_highlights=-12.0,
                lens_distort=18.0, lens_fringe=30.0,
                vignette=38.0, vig_midpoint=45.0, vig_roundness=20.0,
                vig_feather=40.0, vig_hl_contrast=25.0),
        },
        {
            "name": "mask_local",
            "note": "全局 + 局部蒙版增量（含局部清晰度/锐化这类空间项）",
            "image": "structure",
            "params": _dev_params(
                exposure=0.3, contrast=15.0, clarity=20.0,
                masks=[{
                    "id": "g1", "amount": 90.0, "invert": 0,
                    "params": {"exposure": 0.8, "shadows": 30.0,
                               "saturation": -25.0, "clarity": 40.0,
                               "sharp_amount": 55.0},
                    "components": [
                        {"kind": "radial", "x1": 0.45, "y1": 0.42, "x2": 0.75,
                         "y2": 0.42, "feather": 30.0, "midpoint": 50.0,
                         "blend": "add"},
                        {"kind": "linear", "x1": 0.0, "y1": 0.72, "x2": 1.0,
                         "y2": 0.72, "feather": 10.0, "midpoint": 50.0,
                         "blend": "subtract"},
                    ],
                }]),
        },
    ]

    # 单项 case：只开一个运算，其余全默认。这样 Rust 侧每个 stage 能独立验收，
    # 不用等整条链都搬完才发现是谁算错。
    singles = [
        ("tone_only", "smooth",
         "现链已实现的部分：影调六项 + 白平衡 + 去朦胧 + 自然饱和/饱和度 + 主曲线",
         dict(exposure=0.6, contrast=25.0, highlights=-28.0, shadows=24.0,
              whites=14.0, blacks=-16.0, temperature=30.0, tint=-18.0,
              vibrance=22.0, saturation=-12.0, dehaze=20.0, curve_rgb=curve)),
        ("levels_only", "smooth", "色阶五项（输入黑白场 + γ + 输出黑白场）",
         dict(lv_black=18.0, lv_white=238.0, lv_gamma=1.35,
              lv_out_black=8.0, lv_out_white=246.0)),
        ("par_only", "smooth", "参数曲线四段 + 三分界",
         dict(par_shadows=25.0, par_darks=-18.0, par_lights=30.0,
              par_highlights=-22.0, par_split1=20.0, par_split2=55.0,
              par_split3=82.0)),
        ("channel_curves_only", "smooth", "R/G/B 分通道曲线",
         dict(curve_r=curve_r, curve_g=curve_g, curve_b=curve_b)),
        ("hsl_hue_only", "smooth", "HSL 色相偏移（±100 量纲，内部 ×0.4 度）",
         dict(hsl_h=[30.0, -20.0, 0.0, 45.0, -35.0, 15.0, 0.0, -50.0])),
        ("hsl_sat_only", "smooth", "HSL 饱和度",
         dict(hsl_s=[60.0, -40.0, 20.0, 0.0, -70.0, 50.0, 0.0, -25.0])),
        ("hsl_lum_only", "smooth", "HSL 明亮度",
         dict(hsl_l=[0.0, 35.0, -45.0, 12.0, 0.0, -30.0, 55.0, 0.0])),
        ("mono_only", "smooth", "纯转单色（不带混合）", dict(mono=100.0)),
        ("mono_mix_only", "smooth", "单色 + 8 色相带亮度混合",
         dict(mono=100.0, mono_h=[38.0, -22.0, 14.0, 55.0, -30.0, 20.0, -18.0, 33.0])),
        ("grade_sh_only", "smooth", "分级调色：仅阴影",
         dict(grade_sh_h=25.0, grade_sh_s=45.0, grade_sh_l=-10.0)),
        ("grade_hi_only", "smooth", "分级调色：仅高光",
         dict(grade_hi_h=45.0, grade_hi_s=75.0, grade_hi_l=20.0)),
        ("grade_global_only", "smooth", "分级调色：仅全局轮",
         dict(grade_global_h=210.0, grade_global_s=25.0, grade_global_l=8.0)),
        ("grade_balance", "smooth", "均衡挪动三段分割位置",
         dict(grade_sh_h=20.0, grade_sh_s=40.0, grade_hi_h=40.0,
              grade_hi_s=40.0, grade_balance=80.0)),
        ("grade_blending", "smooth", "混合控制三段过渡软硬",
         dict(grade_sh_h=20.0, grade_sh_s=40.0, grade_mid_h=180.0,
              grade_mid_s=25.0, grade_hi_h=40.0, grade_hi_s=40.0,
              grade_blending=85.0)),
        ("clarity_only", "noisy", "清晰度：大半径引导滤波局部对比",
         dict(clarity=55.0)),
        ("clarity_negative", "noisy", "负清晰度（软化）", dict(clarity=-45.0)),
        ("texture_only", "structure", "纹理：小半径局部对比", dict(texture=60.0)),
        ("sharpen_only", "noisy", "锐化（USM + 边缘蒙版 + halo 限制）",
         dict(sharp_amount=80.0, sharp_radius=1.2, sharp_detail=45.0)),
        ("sharpen_masking", "noisy", "锐化 + 蒙版量",
         dict(sharp_amount=80.0, sharp_radius=1.0, sharp_detail=25.0,
              sharp_masking=60.0)),
        ("nr_luma_only", "noisy", "亮度降噪",
         dict(nr_luma=45.0, nr_luma_detail=20.0, nr_luma_contrast=25.0)),
        ("nr_color_only", "noisy", "彩色降噪",
         dict(nr_color=40.0, nr_color_smooth=65.0)),
        ("lens_distort_only", "structure", "镜头畸变 remap",
         dict(lens_distort=38.0)),
        ("lens_distort_negative", "structure", "负畸变", dict(lens_distort=-30.0)),
        ("lens_vignette_only", "structure", "镜头暗角补偿", dict(lens_vignette=-30.0)),
        ("lens_fringe_only", "structure", "去紫边", dict(lens_fringe=60.0)),
        ("vignette_only", "structure", "晕影四项 + 高光保护",
         dict(vignette=45.0, vig_midpoint=42.0, vig_roundness=30.0,
              vig_feather=35.0, vig_hl_contrast=40.0)),
        ("mask_delta_only", "structure", "仅蒙版增量（无局部空间项）",
         dict(masks=[{"id": "m1", "type": "radial", "x1": 0.5, "y1": 0.45,
                      "x2": 0.75, "y2": 0.45, "feather": 25.0, "invert": 0,
                      "angle": 0.0, "midpoint": 50.0, "amount": 100.0,
                      "params": {"exposure": 0.7, "shadows": 25.0,
                                 "saturation": -30.0}}])),
        # —— LR 对齐（下一轮设计 2026-10-30）增补 case ——
        ("exposure_rolloff", "smooth", "曝光高光肩部：大 EV + rolloff，高光不再平顶",
         dict(exposure=1.2, hl_rolloff=60.0)),
        ("black_white_soft", "smooth", "黑白场软拐点：直方图边缘不堆尖，输出恒 [0,1]",
         dict(blacks=-40.0, whites=50.0, black_soft=40.0, white_soft=40.0)),
        ("curve_hdr", "smooth", "曲线 HDR 尾部：>1 的输入按末端斜率延伸而非硬截断",
         dict(exposure=0.9, whites=40.0, curve_hdr_tail=50.0,
              curve_rgb=[[0, 0], [128, 150], [255, 252]])),
        ("linear_contrast", "smooth", "对比度全量走线性光域（contrast_linear=100）",
         dict(contrast=30.0, contrast_linear=100.0)),
        ("sharpen_protect", "noisy", "锐化恒开保护：噪声底之上的平坦区不锐化",
         dict(sharp_amount=60.0, sharp_protect=40.0)),
        # —— 算法里程碑（设计 2026-10-30）增补 case ——
        ("nr_multiscale", "noisy", "多尺度降噪 + 自动 σ（MAD 估计驱动 eps 平方律）",
         dict(nr_luma=40.0, nr_multiscale=60.0)),
        ("nr_multiscale_manual", "noisy", "多尺度降噪 + 手动 σ 覆盖（映射 TODO(fit)）",
         dict(nr_luma=40.0, nr_multiscale=60.0, nr_sigma_manual=45.0)),
        ("dehaze_dark_only", "structure", "暗通道先验去雾（DCP + 大气散射，线性域）",
         dict(dehaze_dark=45.0)),
        ("dehaze_dark_contrast", "structure", "暗通道去雾 + 对比微调（黑位/白位重映射）",
         dict(dehaze_dark=45.0, dehaze_contrast=25.0)),
    ]
    for name, image, note, over in singles:
        cases.append({
            "name": name,
            "note": note,
            "image": image,
            "params": _dev_params(**over),
        })

    images = {k: _dev_image(k) for k in ("smooth", "structure", "noisy")}
    out = {
        "layout": "行优先 H×W×3，小端 float32，hex 编码，取值 [0,1]",
        "size": [DEV_H, DEV_W],
        "images": {
            k: {"w": DEV_W, "h": DEV_H, "hex": _dev_hex(v),
                "stats": _dev_stats(v)}
            for k, v in images.items()
        },
        "cases": [],
    }
    for case in cases:
        src = images[case["image"]]
        res = grade.apply(src, case["params"])
        out["cases"].append({
            "name": case["name"],
            "note": case["note"],
            "stage": _dev_stages(case["params"]),
            "image": case["image"],
            "params": case["params"],
            "out_hex": _dev_hex(res),
            "stats": _dev_stats(res),
        })

    # 颗粒：随机场来自 numpy PCG64，Rust 只能对统计量，不能逐像素比。
    gp = _dev_params(grain=60.0, grain_size=2.0, grain_freq=0.7)
    src = images["smooth"]
    grained = grade.apply(src, gp)
    diff = (grained - src).reshape(-1, 3).astype(np.float64)
    scale = (gp["grain"] / 100.0) * 0.075
    out["cases"].append({
        "name": "grain_stats",
        "note": "颗粒只比统计量：差值 = (grain/100)*0.075*噪声场。"
                "噪声场来自 numpy PCG64 + 下采样模糊，Rust 用自研 RNG，逐像素对不上；"
                "Rust 侧要求 diff_std[i]/diff_scale 落在 Python 值的 ±25% 内、"
                "diff_mean 绝对值 ≤ 0.03*scale、且输出不越界 [0,1]。"
                "size/freq>1/<1 时 std/scale 会明显小于 1（本例 0.7 频率 + size 2 约 0.26）",
        "grain": True,
        "stage": _dev_stages(gp),
        "image": "smooth",
        "params": gp,
        "diff_scale": rnd(scale),
        "diff_mean": [rnd(diff[:, c].mean()) for c in range(3)],
        "diff_std": [rnd(diff[:, c].std()) for c in range(3)],
        "out_clipped": [rnd(float(np.mean((grained <= 0.0) | (grained >= 1.0)))),
                        rnd(float(np.max(grained))), rnd(float(np.min(grained)))],
    })

    # 颗粒亮度调制变体：暗部增强、高光减弱，只比统计量（同上口径）。
    gp2 = _dev_params(grain=60.0, grain_size=2.0, grain_freq=0.7, grain_lum=50.0)
    grained2 = grade.apply(src, gp2)
    diff2 = (grained2 - src).reshape(-1, 3).astype(np.float64)
    out["cases"].append({
        "name": "grain_stats_lum",
        "note": "颗粒亮度调制（grain_lum=50）：差值带 Y 调制乘子，只比统计量，"
                "容差口径与 grain_stats 相同。",
        "grain": True,
        "stage": _dev_stages(gp2),
        "image": "smooth",
        "params": gp2,
        "diff_scale": rnd(scale),
        "diff_mean": [rnd(diff2[:, c].mean()) for c in range(3)],
        "diff_std": [rnd(diff2[:, c].std()) for c in range(3)],
        "out_clipped": [rnd(float(np.mean((grained2 <= 0.0) | (grained2 >= 1.0)))),
                        rnd(float(np.max(grained2))), rnd(float(np.min(grained2)))],
    })
    return out


def mask_snapshot():
    """局部蒙版权重场：几何、布尔组合、强度、反相与亮度范围。"""
    cases = [
        ("linear_default", 9, 9, {
            "id": "m1", "type": "linear",
            "x1": 0.5, "y1": 0.30, "x2": 0.5, "y2": 0.62,
            "feather": 45.0, "invert": 0, "angle": 0.0,
            "midpoint": 50.0, "amount": 100.0,
            "params": {"exposure": 1.0},
        }, None),
        ("linear_tuned", 16, 10, {
            "id": "m2", "type": "linear",
            "x1": 0.2, "y1": 0.15, "x2": 0.8, "y2": 0.75,
            "feather": 30.0, "invert": 1, "angle": 22.0,
            "midpoint": 35.0, "amount": 80.0,
            "params": {"exposure": 0.5},
        }, None),
        ("radial_default", 16, 10, {
            "id": "m3", "type": "radial",
            "x1": 0.5, "y1": 0.5, "x2": 0.72, "y2": 0.5,
            "feather": 35.0, "invert": 0, "angle": 0.0,
            "midpoint": 50.0, "amount": 100.0,
            "params": {"saturation": 20.0},
        }, None),
        ("radial_rotated", 16, 10, {
            "id": "m4", "type": "radial",
            "x1": 0.5, "y1": 0.5, "x2": 0.78, "y2": 0.5,
            "feather": 20.0, "invert": 0, "angle": 30.0,
            "midpoint": 65.0, "amount": 100.0,
            "params": {"exposure": -0.5},
        }, None),
        ("group_subtract", 12, 8, {
            "id": "g1", "amount": 100.0, "invert": 0,
            "params": {"exposure": 0.5},
            "components": [
                {"kind": "linear", "x1": 0.5, "y1": 0.0, "x2": 0.5,
                 "y2": 1.0, "feather": 0.0, "midpoint": 50.0,
                 "blend": "add"},
                {"kind": "radial", "x1": 0.5, "y1": 0.5, "x2": 0.8,
                 "y2": 0.5, "feather": 0.0, "midpoint": 50.0,
                 "blend": "subtract"},
            ],
        }, None),
        ("amount_40", 8, 6, {
            "id": "m5", "type": "radial",
            "x1": 0.5, "y1": 0.5, "x2": 0.9, "y2": 0.5,
            "feather": 0.0, "invert": 0, "angle": 0.0,
            "midpoint": 50.0, "amount": 40.0,
            "params": {"exposure": 1.0},
        }, None),
        ("lum_range", 12, 8, {
            "id": "m6", "amount": 100.0, "invert": 0,
            "params": {"exposure": 0.5},
            "range": {"lum_on": 1, "lum_min": 0.35, "lum_max": 0.8,
                      "lum_feather": 0.1},
            "components": [
                {"kind": "radial", "x1": 0.5, "y1": 0.5, "x2": 4.5,
                 "y2": 0.5, "feather": 0.0, "midpoint": 50.0},
            ],
        }, "ramp"),
    ]
    out = []
    for name, w, h, m, lum_mode in cases:
        lum = None
        if lum_mode == "ramp":
            lum = np.linspace(0.0, 1.0, w * h, dtype=np.float32).reshape(h, w)
        field = grade.mask_field(h, w, m, lum=lum).reshape(-1)
        out.append({
            "name": name, "w": w, "h": h, "mask": m,
            "lum": lum_mode,
            "field": [rnd(v) for v in field],
        })
    return {"cases": out}


def deflicker_snapshot(out: Path, files, gamma_means) -> None:
    """写 deflicker.json：亮度（gamma 与线性两套口径）+ RGB 分通道的输入与偏移。

    gamma 均值是胶片条那套测光，线性均值是去闪真正的输入口径：两边都冻下来，
    Rust 的线性亮路（GUI/MCP 默认）和退回 gamma 的演示路才都有可对的金标。
    """
    def case(means_in, window, strength, multi):
        offsets, stats = deflicker.analyze_means(np.asarray(means_in, float),
                                                window=window,
                                                strength=strength, multi=multi)
        off = np.asarray(offsets)
        return {
            "window": window, "strength": strength, "multi": multi,
            "offsets": ([rnd(v) for v in off] if off.ndim == 1
                        else [[rnd(v) for v in row] for row in off]),
            "stats": stats,
        }

    rgb_means, linear_luma = [], []
    for p in files:
        try:
            im = imio.decode_image(str(p), bps=8, half_size=True)
            rgb_means.append([rnd(v) for v in deflicker.measure_rgb(im)])
            v = deflicker.measure_bgr(im)
            linear_luma.append(None if v is None else rnd(v))
        except Exception as exc:  # 读不出来就留空，不污染基准
            print(f"  去闪测光失败 {p.name}: {exc}", file=sys.stderr)
            rgb_means.append(None)
            linear_luma.append(None)

    cases = [case(gamma_means, *spec) for spec in
             ((15, 100.0, False), (15, 100.0, True),
              (3, 60.0, False), (61, 140.0, False))]
    linear_cases = []
    if all(v is not None for v in linear_luma):
        linear_cases = [case(linear_luma, *spec) for spec in
                        ((15, 100.0, False), (15, 100.0, True),
                         (3, 60.0, False), (61, 140.0, False))]
    color_cases = []
    if all(v is not None for v in rgb_means):
        color_cases = [case(rgb_means, *spec) for spec in
                       ((15, 100.0, False), (15, 100.0, True))]
    (out / "deflicker.json").write_text(json.dumps({
        "frames": [p.name for p in files],
        "input_means": [rnd(v) for v in gamma_means],
        "input_linear_luma": linear_luma,
        "input_rgb": rgb_means,
        "luma_cases": cases,
        "linear_luma_cases": linear_cases,
        "color_cases": color_cases,
    }, ensure_ascii=False, indent=1), encoding="utf-8")
    print(f"写出 {out / 'deflicker.json'}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--frames", help="帧目录；--masks-only 时不需要")
    ap.add_argument("--count", type=int, default=24)
    ap.add_argument("--long-side", type=int, default=512)
    ap.add_argument("--workers", type=int, default=0)
    ap.add_argument("--align-frames", type=int, default=8)
    ap.add_argument("--masks-only", action="store_true",
                    help="只重写 mask.json，不读素材")
    ap.add_argument("--develop-only", action="store_true",
                    help="只重写 develop.json 与 cube.json（合成图，不读素材）")
    ap.add_argument("--cube-only", action="store_true",
                    help="只重写 cube.json")
    ap.add_argument("--deflicker-only", action="store_true",
                    help="只重写 deflicker.json（读素材，不动其他基准）")
    ap.add_argument("--out", default=str(ROOT / "oracle" / "golden"))
    args = ap.parse_args()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)

    if args.cube_only:
        (out / "cube.json").write_text(json.dumps(
            cube_snapshot(), ensure_ascii=False, indent=1), encoding="utf-8")
        print(f"写出 {out / 'cube.json'}")
        return

    if args.develop_only:
        (out / "develop.json").write_text(json.dumps(
            develop_snapshot(), ensure_ascii=False, indent=1), encoding="utf-8")
        (out / "cube.json").write_text(json.dumps(
            cube_snapshot(), ensure_ascii=False, indent=1), encoding="utf-8")
        print(f"写出 {out / 'develop.json'} 与 {out / 'cube.json'}")
        return

    if args.masks_only:
        (out / "mask.json").write_text(json.dumps(
            mask_snapshot(), ensure_ascii=False, indent=1), encoding="utf-8")
        print(f"写出 {out / 'mask.json'}")
        return

    if not args.frames:
        raise SystemExit("需要 --frames（或用 --masks-only）")

    frames_dir = Path(args.frames).resolve()
    files = sorted(p for p in frames_dir.iterdir()
                   if p.is_file() and p.suffix.lower() in IMAGE_EXT)
    files = files[: args.count]
    if not files:
        raise SystemExit(f"没有找到帧：{frames_dir}")

    # 1. 亮度测光
    luma, issues = lumamod.measure_luma(files, long_side=args.long_side,
                                        workers=args.workers)
    means = lumamod.means_of(luma)
    if args.deflicker_only:
        deflicker_snapshot(out, files, means)
        return
    luma_rows = [None if v is None else [rnd(c) for c in v] for v in luma]
    (out / "luma.json").write_text(json.dumps({
        "long_side": args.long_side,
        "frames": [p.name for p in files],
        "luma": luma_rows,
        "means": [rnd(v) if np.isfinite(v) else None for v in means],
        "issues": [[int(i), str(msg)] for i, msg in issues],
    }, ensure_ascii=False, indent=1), encoding="utf-8")

    # 2. 去闪烁：亮度单通道 + RGB 分通道
    deflicker_snapshot(out, files, means)

    # 3. 对齐：模板匹配，8 帧（offsets/scores 是按帧号索引的 dict）
    n_align = min(args.align_frames, len(files))
    offsets, scores, align_issues, elapsed = align.measure_offsets(
        files[:n_align], 0, est="template", workers=args.workers)
    print(f"  对齐 {n_align} 帧用时 {elapsed:.2f}s（不计入基准）")
    (out / "align.json").write_text(json.dumps({
        "est": "template",
        "ref_index": 0,
        "frames": [p.name for p in files[:n_align]],
        "offsets": [[rnd(offsets[i][0]), rnd(offsets[i][1])]
                    for i in range(n_align)],
        "scores": [rnd(scores[i]) for i in range(n_align)],
        "issues": [[int(i), str(m)] for i, m in align_issues],
    }, ensure_ascii=False, indent=1), encoding="utf-8")

    # 4. 调色纯函数
    (out / "grade.json").write_text(json.dumps(
        grade_snapshot(), ensure_ascii=False, indent=1), encoding="utf-8")
    (out / "mask.json").write_text(json.dumps(
        mask_snapshot(), ensure_ascii=False, indent=1), encoding="utf-8")
    (out / "develop.json").write_text(json.dumps(
        develop_snapshot(), ensure_ascii=False, indent=1), encoding="utf-8")
    (out / "cube.json").write_text(json.dumps(
        cube_snapshot(), ensure_ascii=False, indent=1), encoding="utf-8")

    # 5. 元信息（不参与比对，用于排查）
    import cv2
    (out / "meta.json").write_text(json.dumps({
        "frames_dir": str(frames_dir),
        "count": len(files),
        "long_side": args.long_side,
        "python": sys.version.split()[0],
        "numpy": np.__version__,
        "opencv": cv2.__version__,
        "platform": platform.platform(),
        "luma_sha256": digest_floats([c for row in luma if row for c in row]),
    }, ensure_ascii=False, indent=1), encoding="utf-8")

    print(f"写出 {out}")
    for p in sorted(out.glob("*.json")):
        print(f"  {p.name:16s} {p.stat().st_size:8d} B")


if __name__ == "__main__":
    main()
