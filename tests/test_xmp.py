# -*- coding: utf-8 -*-
"""XMP 预设读写：键名映射、归一化刻度、色阶折曲线、蒙版、旧版本提示。

不碰 GUI；真实预设从 %APPDATA%\\Adobe\\CameraRaw\\Settings 取，取不到就跳过那几条。
用法： PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_xmp.py
"""
import copy
import glob
import os
import sys
import tempfile
import xml.etree.ElementTree as ET

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "acceptance"))

from reference import grade as gc            # noqa: E402
from reference import xmp                    # noqa: E402

TMP = os.path.join(HERE, "_tmp", "xmp")
os.makedirs(TMP, exist_ok=True)
FAILS = []


def check(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}"
          + (f"  {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def non_default(p):
    d = gc.defaults()
    return [k for k, v in p.items() if v != d.get(k)]


def rich_params():
    p = gc.defaults()
    p.update({
        "exposure": 0.85, "contrast": 12, "highlights": -30, "shadows": 25,
        "whites": 8, "blacks": -14, "texture": 15, "clarity": 10,
        "dehaze": 7, "vibrance": 18, "saturation": -6,
        "temperature": 22, "tint": -11,
        "hsl_h": [5.0, -3, 2, 4, -6, 7, 8, -9],
        "hsl_s": [-2, 0, 3, 4, 5, 6, -7, -8],
        "hsl_l": [1, 2, 3, 4, 5, 6, 7, 8],
        "mono_h": [1, 2, 3, 4, 5, 6, 7, 8],
        "curve_rgb": [[0, 0], [32, 20], [128, 140], [224, 240], [255, 255]],
        "par_shadows": -12, "par_darks": 8, "par_lights": 6,
        "par_highlights": -4, "par_split1": 30, "par_split2": 55,
        "par_split3": 80,
        "grade_sh_h": 220, "grade_sh_s": 30, "grade_hi_h": 40,
        "grade_hi_s": 20, "grade_mid_h": 120, "grade_mid_s": 10,
        "grade_balance": -15, "grade_sh_l": 5, "grade_mid_l": -3,
        "grade_hi_l": 2, "grade_global_h": 300, "grade_global_s": 7,
        "grade_global_l": -2, "grade_blending": 60,
        "lens_fringe": 35, "vignette": -40, "vig_midpoint": 60,
        "vig_feather": 70, "vig_roundness": 30, "vig_hl_contrast": 45,
        "grain": 22, "sharp_amount": 55, "sharp_radius": 1.1,
        "sharp_detail": 30, "sharp_masking": 12,
        "nr_luma": 18, "nr_luma_detail": 40, "nr_luma_contrast": 5,
        "nr_color": 12, "nr_color_smooth": 35,
    })
    return p


def test_roundtrip():
    p = rich_params()
    path = os.path.join(TMP, "中文预设.xmp")
    notes = xmp.write_xmp(path, p)
    check("导出无遗漏提示", notes == [], str(notes))
    q = xmp.read_xmp(path)

    scalar = ("exposure", "contrast", "highlights", "shadows", "whites",
              "blacks", "texture", "clarity", "dehaze", "vibrance",
              "saturation", "temperature", "tint", "par_shadows", "par_darks",
              "par_lights", "par_highlights", "par_split1", "par_split2",
              "par_split3", "grade_sh_h", "grade_sh_s", "grade_hi_h",
              "grade_hi_s", "grade_mid_h", "grade_mid_s", "grade_balance",
              "grade_sh_l", "grade_mid_l", "grade_hi_l", "grade_global_h",
              "grade_global_s", "grade_global_l", "grade_blending",
              "lens_fringe", "vignette", "vig_midpoint", "vig_feather",
              "vig_roundness", "vig_hl_contrast", "grain", "sharp_amount",
              "sharp_radius", "sharp_detail", "sharp_masking", "nr_luma",
              "nr_luma_detail", "nr_luma_contrast", "nr_color",
              "nr_color_smooth")
    bad = [k for k in scalar if abs(float(p[k]) - float(q[k])) > 0.51]
    check("全局参数往返", not bad,
          "、".join(f"{k}={p[k]}→{q[k]}" for k in bad) or "%d 项" % len(scalar))

    arrs = ("hsl_h", "hsl_s", "hsl_l", "mono_h")
    bad = [k for k in arrs
           if any(abs(a - b) > 1.01 for a, b in zip(p[k], q[k]))
           or len(p[k]) != len(q[k])]
    check("HSL / 黑白混合往返", not bad, "、".join(bad))

    bad = [(k, a, b) for k, a, b in zip(
        ("curve_rgb", "curve_r", "curve_g", "curve_b"),
        (p["curve_rgb"], p["curve_r"], p["curve_g"], p["curve_b"]),
        (q["curve_rgb"], q["curve_r"], q["curve_g"], q["curve_b"]))
        if a != b]
    check("曲线往返", not bad, str(bad))
    check("晕影方向与刻度", q["vignette"] == -40 and q["vig_hl_contrast"] == 45)
    check("中文文件名", q["exposure"] == 0.85 and os.path.exists(path))


def test_levels_fold():
    p = gc.defaults()
    p.update({"curve_rgb": [[0, 0], [128, 120], [255, 255]],
              "lv_black": 16, "lv_gamma": 1.25, "lv_white": 235,
              "lv_out_black": 5, "lv_out_white": 250})
    path = os.path.join(TMP, "levels.xmp")
    xmp.write_xmp(path, p)
    q = xmp.read_xmp(path)
    pts = q["curve_rgb"]
    check("色阶折成 16 点曲线", len(pts) == 16, str(len(pts)))
    lv = gc._levels_lut(p, 4096)
    crv = gc.pchip_lut(p["curve_rgb"], 4096)
    want = [float(crv[int(round(float(lv[int(round(x / 255.0 * 4095))])
                                * 4095))])
            for x in [t[0] for t in pts]]
    err = max(abs(a[1] - b) for a, b in zip(pts, want))
    check("折入后的曲线等效色阶", err <= 1.5, "最大差 %.2f/255" % err)
    check("色阶本身不写 XMP", q["lv_black"] == 0 and q["lv_gamma"] == 1.0)


def mask_group():
    g = gc.new_group("radial")
    g["components"][0].update({"x1": 0.4, "y1": 0.45, "x2": 0.62,
                               "y2": 0.55, "angle": 15.0, "feather": 35})
    g["params"] = {"exposure": 0.5, "saturation": -20, "temperature": 10,
                   "clarity": 49.1, "contrast": -35.5}
    g["amount"] = 80.0
    g2 = gc.new_group("linear")
    g2["components"][0].update({"x1": 0.5, "y1": 0.0, "x2": 0.5, "y2": 0.5})
    g2["params"] = {"exposure": -0.7}
    g2["invert"] = 1
    lin = gc.new_component("linear")
    lin.update({"x1": 0.0, "y1": 0.5, "x2": 0.4, "y2": 0.5,
                "blend": "subtract"})
    g["components"].append(lin)
    return [g, g2]


def test_masks():
    p = gc.defaults()
    p["masks"] = mask_group()
    path = os.path.join(TMP, "masks.xmp")
    notes = xmp.write_xmp(path, p)
    q = xmp.read_xmp(path)
    check("蒙版组数量", len(q["masks"]) == 2, str(len(q["masks"])))
    m0, m1 = q["masks"]
    check("组内多形状", len(m0["components"]) == 2,
          str([c["kind"] for c in m0["components"]]))
    check("形状类型不退化成径向",
          [c["kind"] for c in m0["components"]] == ["radial", "linear"])
    c = m0["components"][0]
    geo = (abs(c["x1"] - 0.4) < 1e-3 and abs(c["y1"] - 0.45) < 1e-3
           and abs(c["x2"] - 0.62) < 1e-3 and abs(c["y2"] - 0.55) < 1e-3
           and abs(c["angle"] - 15.0) < 1e-3)
    check("径向几何往返", geo, str({k: c[k] for k in ("x1", "y1", "x2",
                                                     "y2", "angle")}))
    check("混合模式往返", m0["components"][1]["blend"] == "subtract")
    check("局部参数刻度", abs(m0["params"]["clarity"] - 49.1) < 0.1
          and abs(m0["params"]["contrast"] + 35.5) < 0.1
          and abs(m0["params"]["saturation"] + 20) < 0.1,
          str(m0["params"]))
    check("组强度往返", abs(m0["amount"] - 80.0) < 0.51, str(m0["amount"]))
    check("单形状组反选", m1["components"][0]["invert"] == 1
          and m1["invert"] == 0)

    # 直接看 XML：线性必须写成 Mask/Gradient
    root = ET.parse(path).getroot()
    what = [el.get("{%s}What" % xmp.CRS) for el in root.iter()
            if el.get("{%s}What" % xmp.CRS) in
            ("Mask/Gradient", "Mask/CircularGradient")]
    check("XML 形状标签", what.count("Mask/CircularGradient") == 1
          and what.count("Mask/Gradient") == 2, str(what))
    check("导出的蒙版无多余提示", not [n for n in notes if "没有对应项" in n],
          str(notes))


def test_version_and_errors():
    old = os.path.join(TMP, "old.xmp")
    with open(old, "w", encoding="utf-8") as f:
        f.write("""<?xml version="1.0" encoding="utf-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
   crs:ProcessVersion="5.7" crs:Exposure="0.5" crs:Contrast="10"
   crs:Vibrance="21" crs:Saturation="-16"/>
 </rdf:RDF>
</x:xmpmeta>""")
    p = xmp.read_xmp(old)
    check("旧版本提示", any("旧 ProcessVersion" in n for n in p.notes),
          str(p.notes))
    check("旧版本同义键仍读入", p["exposure"] == 0.5 and p["contrast"] == 10
          and p["saturation"] == -16, f"{p['exposure']},{p['contrast']}")

    bad = os.path.join(TMP, "broken.xmp")
    with open(bad, "w", encoding="utf-8") as f:
        f.write("<not-xmp")
    try:
        xmp.read_xmp(bad)
        ok = False
    except ValueError:
        ok = True
    check("坏文件报错不崩", ok)

    info = xmp.preset_info(old)
    check("预设名与版本探测", info["version"] == "5.7" and info["name"],
          str(info))


def test_real_presets():
    d = xmp.default_preset_dir()
    files = sorted(glob.glob(os.path.join(d, "*.xmp")))
    if not files:
        print("[SKIP] 本机没有 Lightroom 预设目录")
        return
    picked = files[:5]
    ok, rich = 0, 0
    for f in picked:
        p = xmp.read_xmp(f)
        nz = non_default(p)
        ok += bool(nz)
        rich += bool(p["exposure"] or p["contrast"] or any(p["hsl_h"])
                    or any(p["hsl_s"]) or any(p["hsl_l"]))
        print("   读", os.path.basename(f)[:34], "非默认", len(nz))
    check("真实预设都能读出参数", ok == len(picked), f"{ok}/{len(picked)}")
    check("多数含曝光/反差/HSL", rich >= 4, f"{rich}/{len(picked)}")

    mask_files = [f for f in files[:400] if "MaskGroupBasedCorrections" in
                  open(f, encoding="utf-8", errors="ignore").read()]
    legacy = [f for f in files[:400] if "GradientBasedCorrections" in
              open(f, encoding="utf-8", errors="ignore").read()]
    got = 0
    for f in (mask_files[:2] + legacy[:2]):
        p = xmp.read_xmp(f)
        got += bool(p["masks"])
    check("真实预设的蒙版能读", got >= 2, f"{got} 个含蒙版")
    if mask_files:
        p = xmp.read_xmp(mask_files[0])
        kinds = {c["kind"] for g in p["masks"] for c in g["components"]}
        check("现代蒙版族形状", kinds <= {"linear", "radial"} and kinds,
              str(kinds))


def main():
    print("临时目录", TMP)
    test_roundtrip()
    test_levels_fold()
    test_masks()
    test_version_and_errors()
    test_real_presets()
    body = [k for k in os.listdir(TMP) if k.endswith(".xmp")]
    check("导出文件落地", len(body) >= 4, str(sorted(body)))
    print("\n%d 项失败" % len(FAILS) if FAILS else "\n全部通过")
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
