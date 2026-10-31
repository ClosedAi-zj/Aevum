# -*- coding: utf-8 -*-
"""Lightroom / Camera Raw XMP 预设读写。

纯数据转换，不碰 GUI，也不写回 .lrcat（不是 Adobe 的稳定 API）。
crs: 键与 acceptance/reference/grade.py 的参数双向映射；PV2012 没有色阶键，导出时把
色阶采样成曲线点折进 ToneCurvePV2012，读入端无法还原，保持无操作。
"""
import os
import uuid
import xml.etree.ElementTree as ET

import numpy as np

from reference import grade as gc

CRS = "http://ns.adobe.com/camera-raw-settings/1.0/"
RDF = "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
META = "adobe:ns:meta/"

for _prefix, _uri in (("x", META), ("rdf", RDF), ("crs", CRS)):
    ET.register_namespace(_prefix, _uri)

# XMP 的 8 个色相带，顺序与 grade.HSL_NAMES 一致
_BANDS = ("Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple",
          "Magenta")

_GLOBAL = {
    "Exposure2012": "exposure",
    "Contrast2012": "contrast",
    "Highlights2012": "highlights",
    "Shadows2012": "shadows",
    "Whites2012": "whites",
    "Blacks2012": "blacks",
    "Texture": "texture",
    "Clarity2012": "clarity",
    "Dehaze": "dehaze",
    "Vibrance": "vibrance",
    "Saturation": "saturation",
    "Sharpness": "sharp_amount",
    "SharpenRadius": "sharp_radius",
    "SharpenDetail": "sharp_detail",
    "SharpenEdgeMasking": "sharp_masking",
    "LuminanceSmoothing": "nr_luma",
    "LuminanceNoiseReductionDetail": "nr_luma_detail",
    "LuminanceNoiseReductionContrast": "nr_luma_contrast",
    "ColorNoiseReduction": "nr_color",
    "ColorNoiseReductionSmoothness": "nr_color_smooth",
    "PostCropVignetteMidpoint": "vig_midpoint",
    "PostCropVignetteFeather": "vig_feather",
    "GrainAmount": "grain",
}
_GLOBAL_INV = {v: k for k, v in _GLOBAL.items()}

_LOCAL = {
    "LocalExposure2012": "exposure",
    "LocalContrast2012": "contrast",
    "LocalHighlights2012": "highlights",
    "LocalShadows2012": "shadows",
    "LocalWhites2012": "whites",
    "LocalBlacks2012": "blacks",
    "LocalClarity2012": "clarity",
    "LocalDehaze": "dehaze",
    "LocalTemperature": "temperature",
    "LocalTint": "tint",
    "LocalSaturation": "saturation",
}
# 局部校正的 XMP 值归一化到 ±1，只有曝光本身就是档位；全局键才是 ±100
_LOCAL_SCALE = {k: 100.0 for k in _LOCAL.values()}
_LOCAL_SCALE["exposure"] = 1.0

# PV2010 与 2012 同刻度的全局键：新键缺席时兜底，避免老预设读成一片零
_GLOBAL_LEGACY = {"Exposure": "exposure", "Contrast": "contrast",
                  "Vibrance": "vibrance", "Saturation": "saturation"}

_PARAMETRIC = {
    "ParametricShadows": "par_shadows",
    "ParametricDarks": "par_darks",
    "ParametricLights": "par_lights",
    "ParametricHighlights": "par_highlights",
    "ParametricShadowSplit": "par_split1",
    "ParametricMidtoneSplit": "par_split2",
    "ParametricHighlightSplit": "par_split3",
}

_BLEND_TO_XMP = {"add": 0, "subtract": 1, "intersect": 2}
_BLEND_FROM_XMP = {v: k for k, v in _BLEND_TO_XMP.items()}


class XmpParams(dict):
    """读出的参数。notes 是导入提示（旧版本 / 跳过的蒙版）。"""

    def __init__(self, *args, notes=(), **kwargs):
        super().__init__(*args, **kwargs)
        self.notes = list(notes)


def _q(ns, tag):
    return "{%s}%s" % (ns, tag)


def _attrs(el):
    return {k.split("}", 1)[1]: v for k, v in el.attrib.items()
            if k.startswith("{%s}" % CRS)}


def _num(v, default=0.0):
    try:
        return float(str(v).strip())
    except (TypeError, ValueError):
        return float(default)


def _bool(v):
    return str(v).strip().lower() in ("true", "1", "yes")


def _fmt(v):
    if isinstance(v, bool):
        return "True" if v else "False"
    f = float(v)
    if abs(f - round(f)) < 1e-9:
        return str(int(round(f)))
    return ("%.6f" % f).rstrip("0").rstrip(".")


def _clip(v, lo, hi):
    return float(min(max(v, lo), hi))


# ---------------------------------------------------------------- 读
def _read_curve(desc, tag):
    el = desc.find(_q(CRS, tag))
    if el is None:
        return None
    pts = []
    for li in el.iter(_q(RDF, "li")):
        txt = (li.text or "").strip()
        if "," not in txt:
            continue
        x, y = txt.split(",", 1)
        pts.append([int(round(_clip(_num(x), 0, 255))),
                    int(round(_clip(_num(y), 0, 255)))])
    pts.sort(key=lambda t: t[0])
    # 去掉重复横坐标，pchip 要求严格递增
    out = []
    for pt in pts:
        if out and pt[0] == out[-1][0]:
            out[-1] = pt
        else:
            out.append(pt)
    return out if len(out) >= 2 else None


# 三个平级的蒙版族：新格式一个 Correction 可带多个形状；旧格式按形状分族，
# 一个 Correction 只有一个形状。真实预设里旧格式占绝大多数。
_MASK_TAGS = ("MaskGroupBasedCorrections", "GradientBasedCorrections",
              "CircularGradientBasedCorrections")

# PV2010 的局部曝光/反差/清晰度与 2012 同刻度，键名不同；只有亮度语义不同
_LOCAL_LEGACY = {"LocalExposure": "exposure",
                 "LocalContrast": "contrast",
                 "LocalClarity": "clarity"}

_SKIP_LOCAL = {"LocalBrightness": "亮度", "LocalToningHue": "色调分离",
               "LocalToningSaturation": "色调分离",
               "LocalSharpness": "锐化", "LocalMoire": "去摩尔纹",
               "LocalDefringe": "去边", "LocalLuminanceNoise": "降噪"}


def _read_masks(desc, notes):
    groups, skipped, unsupported = [], set(), set()
    range_note = False
    for tag in _MASK_TAGS:
        el = desc.find(_q(CRS, tag))
        if el is None:
            continue
        for li in el.iter(_q(RDF, "li")):
            # 属性写在 li 里还是里面那层 rdf:Description 里，两种都有
            corr = li.find(_q(RDF, "Description"))
            if corr is None:
                corr = li
            if corr.get(_q(CRS, "What")) != "Correction":
                continue
            if str(corr.get(_q(CRS, "CorrectionActive"),
                            "true")).lower() == "false":
                continue
            comps = []
            cms = corr.find(_q(CRS, "CorrectionMasks"))
            for mk in (cms.iter(_q(RDF, "li")) if cms is not None else ()):
                a = _attrs(mk)
                what = a.get("What", "")
                comp = None
                if what == "Mask/Gradient":
                    comp = gc.new_component("linear")
                    comp["x1"] = _num(a.get("FullX"), 0.5)
                    comp["y1"] = _num(a.get("FullY"), 0.0)
                    comp["x2"] = _num(a.get("ZeroX"), 0.5)
                    comp["y2"] = _num(a.get("ZeroY"), 1.0)
                    comp["feather"] = 45.0
                    comp["midpoint"] = 50.0
                elif what == "Mask/CircularGradient":
                    comp = gc.new_component("radial")
                    left = _num(a.get("Left"), 0.0)
                    right = _num(a.get("Right"), 1.0)
                    top = _num(a.get("Top"), 0.0)
                    bottom = _num(a.get("Bottom"), 1.0)
                    comp["x1"] = (left + right) * 0.5
                    comp["y1"] = (top + bottom) * 0.5
                    comp["x2"] = comp["x1"] + (right - left) * 0.5
                    comp["y2"] = comp["y1"] + (bottom - top) * 0.5
                    comp["angle"] = _num(a.get("Angle"))
                    comp["midpoint"] = _clip(_num(a.get("Midpoint"), 50.0),
                                             0, 100)
                    comp["feather"] = _clip(_num(a.get("Feather"), 50.0),
                                            0, 100)
                else:
                    skipped.add(what or "未知")
                    continue
                comp["invert"] = 1 if _bool(a.get("MaskInverted")) else 0
                comp["blend"] = _BLEND_FROM_XMP.get(
                    int(round(_num(a.get("MaskBlendMode")))), "add")
                comps.append(comp)
            if not comps:
                continue
            a = _attrs(corr)
            params = {}
            for xmp_key, our in _LOCAL.items():
                if xmp_key in a:
                    params[our] = _num(a[xmp_key]) * _LOCAL_SCALE[our]
            for xmp_key, our in _LOCAL_LEGACY.items():
                if xmp_key in a and our not in params:
                    params[our] = _num(a[xmp_key]) * _LOCAL_SCALE[our]
            for xmp_key, label in _SKIP_LOCAL.items():
                if abs(_num(a.get(xmp_key))) > 1e-6:
                    unsupported.add(label)
            rm = corr.find(_q(CRS, "CorrectionRangeMask"))
            if (not range_note and rm is not None
                    and str(rm.get(_q(CRS, "Type"), "0")).strip() not in
                    ("", "0")):
                notes.append("范围蒙版没有导入：XMP 的范围类型与这边的亮度范围不同义")
                range_note = True
            groups.append({
                "id": uuid.uuid4().hex[:12],
                "components": comps,
                "params": params,
                "amount": _clip(_num(a.get("CorrectionAmount"), 1.0) * 100,
                                0, 100),
                "invert": 0,
            })
    if skipped:
        notes.append("跳过了不支持的蒙版形状：" + "、".join(sorted(skipped)))
    if unsupported:
        notes.append("局部%s没有对应项，未导入" % "、".join(sorted(unsupported)))
    return groups


def read_xmp(path):
    """读 .xmp 预设，返回补齐默认值后的完整参数字典（附加 .notes 提示）。"""
    try:
        root = ET.parse(path).getroot()
    except ET.ParseError as e:
        raise ValueError("不是有效的 XMP/XML：%s" % e)
    desc = root.find(".//" + _q(RDF, "Description"))
    if desc is None:
        raise ValueError("XMP 里没有 rdf:Description")
    a = _attrs(desc)
    p = gc.defaults()
    notes = []

    pv = a.get("ProcessVersion")
    if pv:
        try:
            major = float(pv)
        except ValueError:
            major = 0.0
        if major < 6.7 - 1e-6:      # 6.7 = PV2012
            notes.append("该预设来自旧 ProcessVersion %s（PV2010 或更早），"
                         "部分参数语义不同，效果会有偏差" % pv)

    for xmp_key, our in _GLOBAL.items():
        if xmp_key in a:
            p[our] = _num(a[xmp_key])
    for xmp_key, our in _GLOBAL_LEGACY.items():
        if xmp_key in a and _GLOBAL_INV.get(our) not in a:
            p[our] = _num(a[xmp_key])

    # LR 的暗角正数提亮四角，这边的正数压暗，方向相反
    p["vignette"] = _clip(-_num(a.get("PostCropVignetteAmount")), -100, 100)
    p["vig_roundness"] = _clip(_num(a.get("PostCropVignetteRoundness")),
                               0, 100)
    p["vig_hl_contrast"] = _clip(
        _num(a.get("PostCropVignetteHighlightContrast")), 0, 100)

    # 白平衡：优先相对偏移；绝对开尔文按 5500K / 45K 每单位近似
    if "IncrementalTemperature" in a:
        p["temperature"] = _clip(_num(a["IncrementalTemperature"]), -100, 100)
    elif "Temperature" in a and a.get("WhiteBalance") == "Custom":
        p["temperature"] = _clip((_num(a["Temperature"]) - 5500.0) / 45.0,
                                 -100, 100)
    if "IncrementalTint" in a:
        p["tint"] = _clip(_num(a["IncrementalTint"]), -100, 100)
    elif "Tint" in a and a.get("WhiteBalance") == "Custom":
        p["tint"] = _clip(_num(a["Tint"]) / 1.5, -100, 100)

    for xmp_key, our in (("HueAdjustment", "hsl_h"),
                         ("SaturationAdjustment", "hsl_s"),
                         ("LuminanceAdjustment", "hsl_l")):
        vals = [_clip(_num(a.get(xmp_key + b), 0.0), -100, 100) for b in _BANDS]
        if any(vals):
            p[our] = vals

    if _bool(a.get("ConvertToGrayscale", "false")):
        p["mono"] = 100.0
    mono = [_clip(_num(a.get("GrayMixer" + b), 0.0), -100, 100) for b in _BANDS]
    if any(mono):
        p["mono_h"] = mono

    for tag, our in (("ToneCurvePV2012", "curve_rgb"),
                     ("ToneCurvePV2012Red", "curve_r"),
                     ("ToneCurvePV2012Green", "curve_g"),
                     ("ToneCurvePV2012Blue", "curve_b")):
        pts = _read_curve(desc, tag)
        if pts:
            p[our] = pts

    for xmp_key, our in _PARAMETRIC.items():
        if xmp_key in a:
            lim = (2.0, 98.0) if our.startswith("par_split") else (-100.0, 100.0)
            p[our] = _clip(_num(a[xmp_key]), *lim)

    for xmp_key, our in (
            ("SplitToningShadowHue", "grade_sh_h"),
            ("ColorGradeShadowHue", "grade_sh_h")):
        if xmp_key in a:
            p[our] = _clip(_num(a[xmp_key]), 0, 360)
    p["grade_sh_s"] = _clip(_num(a.get(
        "SplitToningShadowSaturation", a.get("ColorGradeShadowSat", 0.0))), 0, 100)
    p["grade_mid_h"] = _clip(_num(a.get("ColorGradeMidtoneHue", 0.0)), 0, 360)
    p["grade_mid_s"] = _clip(_num(a.get("ColorGradeMidtoneSat", 0.0)), 0, 100)
    for xmp_key, our in (
            ("SplitToningHighlightHue", "grade_hi_h"),
            ("ColorGradeHighlightHue", "grade_hi_h")):
        if xmp_key in a:
            p[our] = _clip(_num(a[xmp_key]), 0, 360)
    p["grade_hi_s"] = _clip(_num(a.get(
        "SplitToningHighlightSaturation",
        a.get("ColorGradeHighlightSat", 0.0))), 0, 100)
    p["grade_sh_l"] = _clip(_num(a.get("ColorGradeShadowLum", 0.0)), -100, 100)
    p["grade_mid_l"] = _clip(_num(a.get("ColorGradeMidtoneLum", 0.0)), -100, 100)
    p["grade_hi_l"] = _clip(_num(a.get("ColorGradeHighlightLum", 0.0)), -100, 100)
    p["grade_global_h"] = _clip(_num(a.get("ColorGradeGlobalHue", 0.0)), 0, 360)
    p["grade_global_s"] = _clip(_num(a.get("ColorGradeGlobalSat", 0.0)), 0, 100)
    p["grade_global_l"] = _clip(_num(a.get("ColorGradeGlobalLum", 0.0)), -100, 100)
    p["grade_balance"] = _clip(_num(a.get("SplitToningBalance", 0.0)), -100, 100)
    p["grade_blending"] = _clip(_num(a.get("ColorGradeBlending", 0.0)), 0, 100)

    fringe = max(_num(a.get("DefringePurpleAmount"), 0.0),
                 _num(a.get("DefringeGreenAmount"), 0.0))
    if fringe:
        p["lens_fringe"] = _clip(fringe / 20.0 * 100.0, 0, 100)

    p["masks"] = _read_masks(desc, notes)
    return XmpParams(p, notes=notes)


# ---------------------------------------------------------------- 写
def levels_to_curve_points(p, n=16):
    """把色阶采样成曲线点；曲线与色阶同时开时按流水线顺序合在一起。"""
    lut = gc.pchip_lut(p.get("curve_rgb", [[0, 0], [255, 255]]), 4096)
    lv = gc._levels_lut(p, 4096) if gc._levels_active(p) else None
    pts = []
    for i in range(int(n)):
        x = i / (n - 1.0) * 255.0
        if lv is not None:
            y = float(lut[int(round(float(lv[int(round(x / 255.0 * 4095))])
                                    * 4095))])
        else:
            y = float(lut[int(round(x / 255.0 * 4095))])
        pts.append([int(round(x)), int(round(min(max(y, 0.0), 255.0)))])
    return pts


def _write_curve(desc, tag, pts):
    el = ET.SubElement(desc, _q(CRS, tag))
    seq = ET.SubElement(el, _q(RDF, "Seq"))
    for x, y in pts:
        li = ET.SubElement(seq, _q(RDF, "li"))
        li.text = "%d, %d" % (int(round(x)), int(round(y)))


def _rot_linear(c):
    """把 angle 烘进端点（XMP 的线性渐变只有 Zero/Full，没有角度）。"""
    x1, y1 = float(c["x1"]), float(c["y1"])
    x2, y2 = float(c["x2"]), float(c["y2"])
    ang = float(c.get("angle") or 0.0)
    if not ang:
        return x1, y1, x2, y2
    rad = np.deg2rad(ang)
    co, si = float(np.cos(rad)), float(np.sin(rad))
    mx, my = (x1 + x2) * 0.5, (y1 + y2) * 0.5
    dx, dy = x1 - mx, y1 - my
    x1, y1 = mx + dx * co - dy * si, my + dx * si + dy * co
    dx, dy = x2 - mx, y2 - my
    x2, y2 = mx + dx * co - dy * si, my + dx * si + dy * co
    return x1, y1, x2, y2


def _write_comp(li, comp, notes, gi, ci):
    a = li
    a.set(_q(CRS, "MaskActive"), "true")
    a.set(_q(CRS, "MaskBlendMode"),
          str(_BLEND_TO_XMP.get(comp.get("blend", "add"), 0)))
    a.set(_q(CRS, "MaskInverted"),
          "true" if comp.get("invert") else "false")
    a.set(_q(CRS, "MaskValue"), "1")
    a.set(_q(CRS, "MaskName"), "蒙版 %d 形状 %d" % (gi, ci))
    if (comp.get("kind") or comp.get("type")) == "radial":
        cx, cy = float(comp["x1"]), float(comp["y1"])
        rx = abs(float(comp["x2"]) - cx)
        ry = abs(float(comp["y2"]) - cy)
        if ry < 0.01:
            ry = rx
        elif rx < 0.01:
            rx = ry
        a.set(_q(CRS, "What"), "Mask/CircularGradient")
        a.set(_q(CRS, "Top"), _fmt(cy - ry))
        a.set(_q(CRS, "Left"), _fmt(cx - rx))
        a.set(_q(CRS, "Bottom"), _fmt(cy + ry))
        a.set(_q(CRS, "Right"), _fmt(cx + rx))
        a.set(_q(CRS, "Angle"), _fmt(comp.get("angle") or 0.0))
        a.set(_q(CRS, "Midpoint"),
              "%+g" % _clip(comp.get("midpoint", 50.0), 0, 100))
        a.set(_q(CRS, "Roundness"), "0")
        a.set(_q(CRS, "Feather"),
              "%+g" % _clip(comp.get("feather", 50.0), 0, 100))
        a.set(_q(CRS, "Flipped"), "false")
        a.set(_q(CRS, "Version"), "+2")
    else:
        x1, y1, x2, y2 = _rot_linear(comp)
        a.set(_q(CRS, "What"), "Mask/Gradient")
        a.set(_q(CRS, "FullX"), _fmt(x1))
        a.set(_q(CRS, "FullY"), _fmt(y1))
        a.set(_q(CRS, "ZeroX"), _fmt(x2))
        a.set(_q(CRS, "ZeroY"), _fmt(y2))
        if abs(float(comp.get("feather", 45.0)) - 45.0) > 1e-6:
            notes.append("线性渐变的羽化在 Lightroom 里固定，XMP 没有对应键")


def _write_masks(desc, masks, notes):
    if not masks:
        return
    el = ET.SubElement(desc, _q(CRS, "MaskGroupBasedCorrections"))
    seq = ET.SubElement(el, _q(RDF, "Seq"))
    for i, g in enumerate(masks, 1):
        corr = ET.SubElement(ET.SubElement(seq, _q(RDF, "li")),
                             _q(RDF, "Description"))
        corr.set(_q(CRS, "What"), "Correction")
        amt = _clip(g.get("amount", 100.0) if "components" in g
                    else 100.0, 0, 100)
        corr.set(_q(CRS, "CorrectionAmount"), _fmt(amt / 100.0))
        corr.set(_q(CRS, "CorrectionActive"), "true")
        corr.set(_q(CRS, "CorrectionName"), "蒙版 %d" % i)
        params = g.get("params") or {}
        for xmp_key, our in _LOCAL.items():
            corr.set(_q(CRS, xmp_key),
                     _fmt(params.get(our, 0.0) / _LOCAL_SCALE[our]))
        unsupported = [label for k, label in (
            ("vibrance", "自然饱和度"), ("texture", "纹理"),
            ("sharp_amount", "锐化"), ("nr_luma", "降噪"),
            ("nr_color", "色彩降噪"), ("nr_luma_detail", "降噪细节"),
            ("nr_luma_contrast", "降噪对比"), ("nr_color_smooth", "降噪平滑"))
            if abs(float(params.get(k, 0.0) or 0.0)) > 1e-6]
        if unsupported:
            notes.append("蒙版 %d 的%s在 Lightroom 局部调整里没有对应项"
                         % (i, "、".join(unsupported)))
        rm = g.get("range") or {}
        if rm.get("lum_on"):
            notes.append("蒙版 %d 的亮度范围没有写进 XMP" % i)
        comps = g.get("components") or [dict(g, kind=g.get("type", "linear"))]
        if g.get("invert"):
            if len(comps) == 1:
                c = dict(comps[0])
                c["invert"] = 0 if c.get("invert") else 1
                comps = [c]
            else:
                notes.append("蒙版 %d 的整组反选无法写进 XMP（分形状反选不同义），"
                             "已按未反选导出" % i)
        cms = ET.SubElement(corr, _q(CRS, "CorrectionMasks"))
        mseq = ET.SubElement(cms, _q(RDF, "Seq"))
        for j, comp in enumerate(comps, 1):
            _write_comp(ET.SubElement(mseq, _q(RDF, "li")), comp, notes, i, j)


def write_xmp(path, p):
    """写 .xmp 预设；返回没有写入的项（连字符形 vs 不同义）。"""
    notes = []
    root = ET.Element(_q(META, "xmpmeta"), {_q(META, "xmptk"): "lapse"})
    rdf = ET.SubElement(root, _q(RDF, "RDF"))
    d = ET.SubElement(rdf, _q(RDF, "Description"))
    d.set(_q(CRS, "PresetType"), "Normal")
    d.set(_q(CRS, "Version"), "17.3.1")
    d.set(_q(CRS, "ProcessVersion"), "15.4")
    d.set(_q(CRS, "HasSettings"), "True")

    for xmp_key, our in _GLOBAL.items():
        d.set(_q(CRS, xmp_key), _fmt(p.get(our, 0.0)))

    d.set(_q(CRS, "PostCropVignetteAmount"),
          _fmt(-_clip(p.get("vignette", 0.0), -100, 100)))
    hc = _clip(p.get("vig_hl_contrast", 0.0), -100, 100)
    if hc < 0:
        notes.append("晕影高光保护的负值在 Lightroom 里没有对应档位，按 0 导出")
        hc = 0.0
    d.set(_q(CRS, "PostCropVignetteHighlightContrast"), _fmt(hc))
    d.set(_q(CRS, "PostCropVignetteRoundness"),
          _fmt(_clip(p.get("vig_roundness", 0.0), 0, 100)))

    if abs(float(p.get("temperature", 0.0))) > 1e-6 or \
            abs(float(p.get("tint", 0.0))) > 1e-6:
        d.set(_q(CRS, "WhiteBalance"), "Custom")
        d.set(_q(CRS, "IncrementalTemperature"),
              _fmt(_clip(p.get("temperature", 0.0), -100, 100)))
        d.set(_q(CRS, "IncrementalTint"),
              _fmt(_clip(p.get("tint", 0.0), -100, 100)))

    for xmp_key, our in (("HueAdjustment", "hsl_h"),
                         ("SaturationAdjustment", "hsl_s"),
                         ("LuminanceAdjustment", "hsl_l")):
        vals = p.get(our) or [0.0] * 8
        for i, band in enumerate(_BANDS):
            d.set(_q(CRS, xmp_key + band),
                  _fmt(_clip(vals[i] if i < len(vals) else 0.0, -100, 100)))

    mono = float(p.get("mono", 0.0) or 0.0)
    d.set(_q(CRS, "ConvertToGrayscale"), "True" if mono > 0 else "False")
    if 0 < mono < 100:
        notes.append("Lightroom 只有黑白开关，单色 %.0f%% 按全黑白导出" % mono)
    mix = p.get("mono_h") or [0.0] * 8
    for i, band in enumerate(_BANDS):
        d.set(_q(CRS, "GrayMixer" + band),
              _fmt(_clip(mix[i] if i < len(mix) else 0.0, -100, 100)))

    _write_curve(d, "ToneCurvePV2012", levels_to_curve_points(p, 16)
                 if gc._levels_active(p) else p.get("curve_rgb"))
    for tag, key in (("ToneCurvePV2012Red", "curve_r"),
                     ("ToneCurvePV2012Green", "curve_g"),
                     ("ToneCurvePV2012Blue", "curve_b")):
        _write_curve(d, tag, p.get(key) or [[0, 0], [255, 255]])

    for xmp_key, our in _PARAMETRIC.items():
        d.set(_q(CRS, xmp_key), _fmt(p.get(our, 0.0)))

    d.set(_q(CRS, "SplitToningShadowHue"), _fmt(p.get("grade_sh_h", 0.0)))
    d.set(_q(CRS, "SplitToningShadowSaturation"),
          _fmt(p.get("grade_sh_s", 0.0)))
    d.set(_q(CRS, "SplitToningHighlightHue"), _fmt(p.get("grade_hi_h", 0.0)))
    d.set(_q(CRS, "SplitToningHighlightSaturation"),
          _fmt(p.get("grade_hi_s", 0.0)))
    d.set(_q(CRS, "SplitToningBalance"), _fmt(p.get("grade_balance", 0.0)))
    d.set(_q(CRS, "ColorGradeMidtoneHue"), _fmt(p.get("grade_mid_h", 0.0)))
    d.set(_q(CRS, "ColorGradeMidtoneSat"), _fmt(p.get("grade_mid_s", 0.0)))
    d.set(_q(CRS, "ColorGradeShadowLum"), _fmt(p.get("grade_sh_l", 0.0)))
    d.set(_q(CRS, "ColorGradeMidtoneLum"), _fmt(p.get("grade_mid_l", 0.0)))
    d.set(_q(CRS, "ColorGradeHighlightLum"), _fmt(p.get("grade_hi_l", 0.0)))
    d.set(_q(CRS, "ColorGradeBlending"), _fmt(p.get("grade_blending", 0.0)))
    d.set(_q(CRS, "ColorGradeGlobalHue"), _fmt(p.get("grade_global_h", 0.0)))
    d.set(_q(CRS, "ColorGradeGlobalSat"), _fmt(p.get("grade_global_s", 0.0)))
    d.set(_q(CRS, "ColorGradeGlobalLum"), _fmt(p.get("grade_global_l", 0.0)))

    fringe = _clip(p.get("lens_fringe", 0.0), 0, 100) / 100.0 * 20.0
    d.set(_q(CRS, "DefringePurpleAmount"), _fmt(fringe))
    d.set(_q(CRS, "DefringeGreenAmount"), _fmt(fringe))

    grain = _clip(p.get("grain", 0.0), 0, 100)
    d.set(_q(CRS, "GrainAmount"), _fmt(grain))
    if grain > 0 and (abs(float(p.get("grain_size", 1.0)) - 1.0) > 1e-6
                      or abs(float(p.get("grain_freq", 1.0)) - 1.0) > 1e-6):
        notes.append("颗粒大小 / 密度在 Lightroom 里是另一套刻度，没有写入")

    _write_masks(d, p.get("masks") or [], notes)

    alt = ET.SubElement(d, _q(CRS, "Name"))
    li = ET.SubElement(ET.SubElement(alt, _q(RDF, "Alt")), _q(RDF, "li"))
    li.set("{http://www.w3.org/XML/1998/namespace}lang", "x-default")
    li.text = os.path.splitext(os.path.basename(path))[0]

    ET.ElementTree(root).write(path, encoding="utf-8", xml_declaration=True)
    return notes


# ---------------------------------------------------------------- 目录
def default_preset_dir():
    """LR / Camera Raw 的预设目录（只用于打开对话框的起始位置）。"""
    appdata = os.environ.get("APPDATA")
    if appdata:
        d = os.path.join(appdata, "Adobe", "CameraRaw", "Settings")
        if os.path.isdir(d):
            return d
    return os.path.expanduser("~")


def preset_info(path):
    """快速读取预设名与版本，给文件对话框/列表用。"""
    try:
        root = ET.parse(path).getroot()
        desc = root.find(".//" + _q(RDF, "Description"))
        a = _attrs(desc) if desc is not None else {}
        name = a.get("Name") or os.path.splitext(os.path.basename(path))[0]
        return {"name": name, "version": a.get("ProcessVersion", "")}
    except (ET.ParseError, OSError):
        return {"name": os.path.splitext(os.path.basename(path))[0],
                "version": ""}
