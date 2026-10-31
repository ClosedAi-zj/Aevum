"""调色参数模型 + 撤销/重做历史。

参数结构与 acceptance/reference/grade.py 完全一致；本模块只负责存取与历史，不做像素运算。
"""
import copy

from PySide6 import QtCore

from reference import grade as gc

# (键, 显示名, 最小, 最大, 步进, 格式, 方向说明)
# hsl_* 用 ("hsl_s", 下标) 定位；方向说明进 tooltip —— 控件必须说清往哪边走是什么
SLIDER_SPEC = [
    ("基本", [
        ("exposure", "曝光", -4.0, 4.0, 0.01, "{:+.2f}",
         "左 = 压暗，右 = 提亮；一格约等于一档光圈"),
        ("contrast", "对比度", -100.0, 100.0, 1, "{:+.0f}",
         "右 = 中间调更陡（亮的更亮、暗的更暗），左 = 变平"),
        ("highlights", "高光", -100.0, 100.0, 1, "{:+.0f}",
         "右 = 提亮高光，左 = 压回高光细节"),
        ("shadows", "阴影", -100.0, 100.0, 1, "{:+.0f}",
         "右 = 提亮暗部，左 = 压暗暗部"),
        ("whites", "白色色阶", -100.0, 100.0, 1, "{:+.0f}",
         "右 = 抬高白场（最亮处更亮），左 = 收白场"),
        ("blacks", "黑色色阶", -100.0, 100.0, 1, "{:+.0f}",
         "右 = 抬高黑场（暗部发灰），左 = 压黑（暗部更实）"),
        ("texture", "纹理", -100.0, 100.0, 1, "{:+.0f}",
         "右 = 强化中频细节，左 = 抹平皮肤与织物"),
        ("clarity", "清晰度", -100.0, 100.0, 1, "{:+.0f}",
         "右 = 中间调局部对比增强（更硬朗），左 = 柔化"),
        ("vibrance", "自然饱和度", -100.0, 100.0, 1, "{:+.0f}",
         "右 = 只提低饱和的颜色，肤色不容易溢出"),
        ("saturation", "饱和度", -100.0, 100.0, 1, "{:+.0f}",
         "右 = 全通道加饱和，左 = 一路去色到黑白"),
        ("temperature", "色温", -100.0, 100.0, 1, "{:+.0f}",
         "左 = 偏冷蓝，右 = 偏暖黄"),
        ("tint", "色调", -100.0, 100.0, 1, "{:+.0f}",
         "左 = 偏绿，右 = 偏品红"),
    ]),
    ("效果", [
        ("vignette", "晕影", -100.0, 100.0, 1, "{:+.0f}",
         "右 = 压暗四角，左 = 提亮四角"),
        ("vig_midpoint", "晕影中点", 0.0, 100.0, 1, "{:.0f}",
         "压暗从哪里开始：左 = 从边缘开始，右 = 一直收到中心附近"),
        ("vig_roundness", "晕影形状", 0.0, 100.0, 1, "{:.0f}",
         "左 = 跟随画幅的椭圆，右 = 正圆（宽画幅左右收得更紧）"),
        ("vig_feather", "晕影羽化", 0.0, 100.0, 1, "{:.0f}",
         "右 = 过渡带更宽更柔，左 = 硬边"),
        ("vig_hl_contrast", "晕影高光保护", -100.0, 100.0, 1, "{:+.0f}",
         "右 = 亮部少压暗（收边不发脏），左 = 亮部也一起压"),
        ("grain", "颗粒", 0.0, 100.0, 1, "{:.0f}",
         "左 = 干净无颗粒，右 = 胶片颗粒越明显"),
        ("grain_size", "颗粒大小", 1.0, 8.0, 0.5, "{:.1f}",
         "右 = 颗粒更粗（像素半径）；1 = 最细"),
        ("grain_freq", "颗粒密度", 0.2, 1.0, 0.05, "{:.2f}",
         "左 = 网格更粗（块状胶片颗粒），右 = 逐像素细噪点"),
        ("dehaze", "去朦胧", -100.0, 100.0, 1, "{:+.0f}",
         "右 = 去雾更通透（黑位下压），左 = 加雾发灰"),
    ]),
    ("细节", [
        ("sharp_amount", "锐化", 0.0, 150.0, 1, "{:.0f}",
         "右 = 边缘更锐；0 = 不锐化"),
        ("sharp_radius", "锐化半径", 0.5, 3.0, 0.1, "{:.1f}",
         "半径越大轮廓越粗；高像素图适当加大"),
        ("sharp_detail", "锐化细节", 0.0, 100.0, 1, "{:.0f}",
         "右 = 强调更细的高频，左 = 只锐化大边缘"),
        ("sharp_masking", "锐化蒙版", 0.0, 100.0, 1, "{:.0f}",
         "右 = 只锐化明显边缘，保护皮肤与平滑区域"),
        ("nr_luma", "亮度降噪", 0.0, 100.0, 1, "{:.0f}",
         "右 = 抹平亮度噪点；过高会丢失细节"),
        ("nr_color", "色彩降噪", 0.0, 100.0, 1, "{:.0f}",
         "右 = 抹平彩色噪点，适合高感光与暗部"),
        ("nr_luma_detail", "降噪细节", 0.0, 100.0, 1, "{:.0f}",
         "右 = 保住星星与纹理边缘，只磨真正的噪点"),
        ("nr_luma_contrast", "降噪对比", 0.0, 100.0, 1, "{:.0f}",
         "右 = 把磨掉的局部对比还回来，画面不发糊"),
        ("nr_color_smooth", "降噪平滑", 0.0, 100.0, 1, "{:.0f}",
         "右 = 彩色噪点磨得更平（色块更干净），左 = 保色细节"),
    ]),
    ("色彩分级", [
        ("grade_global_h", "全局色相", 0.0, 360.0, 1, "{:.0f}",
         "所有亮度统一偏色：0=红 60=黄 120=绿 240=蓝"),
        ("grade_global_s", "全局饱和", 0.0, 100.0, 1, "{:.0f}",
         "右 = 整幅图统一染色更浓；0 = 不染色"),
        ("grade_global_l", "全局亮度", -100.0, 100.0, 1, "{:+.0f}",
         "整幅图随全局染色一起提亮 / 压暗"),
        ("grade_sh_h", "阴影色相", 0.0, 360.0, 1, "{:.0f}",
         "阴影染色的色相：0=红 60=黄 120=绿 240=蓝"),
        ("grade_sh_s", "阴影饱和", 0.0, 100.0, 1, "{:.0f}",
         "右 = 阴影染色更浓；0 = 不染色"),
        ("grade_sh_l", "阴影亮度", -100.0, 100.0, 1, "{:+.0f}",
         "阴影区随染色提亮 / 压暗"),
        ("grade_mid_h", "中间调色相", 0.0, 360.0, 1, "{:.0f}",
         "中间调染色的色相"),
        ("grade_mid_s", "中间调饱和", 0.0, 100.0, 1, "{:.0f}",
         "右 = 中间调染色更浓"),
        ("grade_mid_l", "中间调亮度", -100.0, 100.0, 1, "{:+.0f}",
         "中间调随染色提亮 / 压暗"),
        ("grade_hi_h", "高光色相", 0.0, 360.0, 1, "{:.0f}",
         "高光染色的色相"),
        ("grade_hi_s", "高光饱和", 0.0, 100.0, 1, "{:.0f}",
         "右 = 高光染色更浓"),
        ("grade_hi_l", "高光亮度", -100.0, 100.0, 1, "{:+.0f}",
         "高光区随染色提亮 / 压暗"),
        ("grade_balance", "分级平衡", -100.0, 100.0, 1, "{:+.0f}",
         "左 = 阴影区更大，右 = 高光区更大"),
        ("grade_blending", "分级过渡", 0.0, 100.0, 1, "{:.0f}",
         "左 = 三段硬分割（旧观感），右 = 按亮度完全平滑融合"),
    ]),
    # 色阶用画布直接拖（见 panels.LevelsCanvas），这里只留黑白混合与镜头校正
    ("黑白", [
        ("mono", "单色", 0.0, 100.0, 1, "{:.0f}",
         "右 = 完全转黑白；中间值 = 彩色与黑白按此比例混合"),
    ]),
    ("镜头校正", [
        ("lens_distort", "畸变", -100.0, 100.0, 1, "{:+.0f}",
         "右 = 校正桶形畸变（边缘向外推），左 = 反向枕形"),
        ("lens_vignette", "暗角补偿", -100.0, 100.0, 1, "{:+.0f}",
         "右 = 提亮四角（补回镜头暗角），左 = 压暗四角"),
        ("lens_fringe", "去紫边", 0.0, 100.0, 1, "{:.0f}",
         "右 = 削弱高反差边缘的紫边与绿边"),
    ]),
]

# 色阶画布（见 panels.LevelsCanvas）编辑的这五个键：分组的「复位」和画布
# 双击复位共用，别在两处各写一遍
LEVEL_KEYS = ("lv_black", "lv_white", "lv_gamma",
              "lv_out_black", "lv_out_white")

# 参数量程：滑块按 SLIDER_SPEC 建，别的入口（拖亮度曲线改曝光）用这张表夹住，
# 免得两处各写一遍上下限，改了滑块忘了改那边。
LIMITS = {s[0]: (float(s[2]), float(s[3]))
          for _title, spec in SLIDER_SPEC for s in spec}

# 参数曲线：4 个亮度区 + 3 个分割点，与 Lightroom 的 Parametric 模式同义。
# 分割点约束了顺序，滑块打到极端时 grade._par_lut 还会再夹一次。
PARAM_SLIDERS = [
    ("par_shadows", "阴影区", -100.0, 100.0, 1, "{:+.0f}",
     "右 = 提亮最暗的一段，左 = 压暗"),
    ("par_darks", "暗部区", -100.0, 100.0, 1, "{:+.0f}",
     "右 = 提亮暗部中间调，左 = 压暗"),
    ("par_lights", "亮部区", -100.0, 100.0, 1, "{:+.0f}",
     "右 = 提亮亮部中间调，左 = 压暗"),
    ("par_highlights", "高光区", -100.0, 100.0, 1, "{:+.0f}",
     "右 = 提亮最亮的一段，左 = 压回高光细节"),
    ("par_split1", "阴影分割", 0.0, 100.0, 1, "{:.0f}",
     "阴影区与暗部区的分界（默认 25）"),
    ("par_split2", "中间分割", 0.0, 100.0, 1, "{:.0f}",
     "暗部区与亮部区的分界（默认 50）"),
    ("par_split3", "高光分割", 0.0, 100.0, 1, "{:.0f}",
     "亮部区与高光区的分界（默认 75）"),
]
PARAM_KEYS = tuple(s[0] for s in PARAM_SLIDERS)

# 黑白混合：8 个色相带的亮度权重，与 HSL 同一套色相带划分
MONO_BANDS = [
    ("红", "红系变亮（肤色、秋叶）"),
    ("橙", "橙系变亮（暖色中间调）"),
    ("黄", "黄系变亮（灯光、麦田）"),
    ("绿", "绿系变亮（树叶；往下压更沉）"),
    ("浅绿", "青绿系变亮（水面、松针）"),
    ("蓝", "蓝系变亮；往下压 = 天空更透"),
    ("紫", "紫系变亮"),
    ("洋红", "洋红系变亮（肤色偏红的部分）"),
]

# 局部蒙版能带的参数：全部是「相对全局的增量」，与 reference.grade.MASK_KEYS 一致
MASK_SLIDERS = [
    ("exposure", "曝光", -4.0, 4.0, 0.01, "{:+.2f}",
     "相对全局：右 = 这块区域更亮，左 = 更暗"),
    ("contrast", "对比度", -100.0, 100.0, 1, "{:+.0f}", "相对全局的中间调陡度"),
    ("highlights", "高光", -100.0, 100.0, 1, "{:+.0f}", "只影响这块区域的高光"),
    ("shadows", "阴影", -100.0, 100.0, 1, "{:+.0f}", "只影响这块区域的暗部"),
    ("whites", "白色色阶", -100.0, 100.0, 1, "{:+.0f}", "只影响这块区域的白场"),
    ("blacks", "黑色色阶", -100.0, 100.0, 1, "{:+.0f}", "只影响这块区域的黑场"),
    ("dehaze", "去朦胧", -100.0, 100.0, 1, "{:+.0f}",
     "右 = 这块更通透，压天空很有效"),
    ("clarity", "清晰度", -100.0, 100.0, 1, "{:+.0f}", "只影响这块区域的局部对比"),
    ("texture", "纹理", -100.0, 100.0, 1, "{:+.0f}", "只影响这块区域的中频细节"),
    ("vibrance", "自然饱和度", -100.0, 100.0, 1, "{:+.0f}", "只提这块区域的低饱和颜色"),
    ("saturation", "饱和度", -100.0, 100.0, 1, "{:+.0f}", "这块区域整体加/减饱和"),
    ("temperature", "色温", -100.0, 100.0, 1, "{:+.0f}",
     "相对全局：右 = 这块更暖，左 = 更冷"),
    ("tint", "色调", -100.0, 100.0, 1, "{:+.0f}", "相对全局：右 = 偏品红"),
]

# 局部空间项：不重跑局部整条管线，合成后按组权重单遍执行（M03）。
# 半径/细节/蒙版只有在该组锐化 > 0 时才参与。
MASK_SPATIAL_SLIDERS = [
    ("sharp_amount", "局部锐化", 0.0, 150.0, 1, "{:.0f}",
     "只在这块区域锐化；合成后单遍执行，不会二次压暗白边"),
    ("sharp_radius", "锐化半径", 0.5, 3.0, 0.1, "{:.1f}",
     "半径越大轮廓越粗"),
    ("sharp_detail", "锐化细节", 0.0, 100.0, 1, "{:.0f}",
     "右 = 强调更细的高频"),
    ("sharp_masking", "锐化蒙版", 0.0, 100.0, 1, "{:.0f}",
     "右 = 只锐化明显边缘"),
    ("nr_luma", "局部降噪", 0.0, 100.0, 1, "{:.0f}",
     "只抹这块区域的亮度噪点"),
    ("nr_color", "局部彩噪", 0.0, 100.0, 1, "{:.0f}",
     "只抹这块区域的彩色噪点"),
]


class Store(QtCore.QObject):
    """当前参数状态 + 编辑历史。"""

    changed = QtCore.Signal(object)      # 任意参数变化（携带来源标签）
    history_changed = QtCore.Signal()

    def __init__(self, parent=None):
        super().__init__(parent)
        self.p = gc.defaults()
        self._undo = []
        self._redo = []
        self._pending = None

    # ---- 读写 ----
    def value(self, key, idx=None):
        if idx is None:
            return self.p.get(key, 0.0)
        return self.p[key][idx]

    def set(self, key, value, idx=None, source="ui"):
        cur = self.value(key, idx)
        if cur == value:
            return
        if idx is None:
            self.p[key] = value
        else:
            self.p[key][idx] = value
        self.changed.emit(source)

    # ---- 局部蒙版组 ----
    # 组 = 多个形状组件的布尔组合 + 一套增量 + 整体强度。
    # 参数存的是「相对全局的增量」，所以中性值永远是 0，复位也写 0。
    GEOM_ATTRS = ("x1", "y1", "x2", "y2", "feather", "angle",
                  "midpoint", "blend", "kind", "type")

    def mask_count(self):
        return len(self.p.get("masks") or ())

    def mask(self, i):
        ms = self.p.get("masks") or ()
        return ms[i] if 0 <= i < len(ms) else None

    def comp_count(self, i):
        m = self.mask(i)
        if m is None:
            return 0
        return len(m.get("components") or [m])

    def comp(self, i, j=0):
        m = self.mask(i)
        if m is None:
            return None
        cs = m.get("components") or [m]
        return cs[j] if 0 <= j < len(cs) else None

    def mask_value(self, i, key):
        m = self.mask(i)
        d = (m or {}).get("params") or {}
        if key in d:
            return d[key]
        # 空间项没填过时按全局默认值显示：半径 1.0 之类不能显示成 0
        if key in gc.MASK_SPATIAL:
            return float(gc.DEFAULT.get(key, 0.0))
        return 0.0

    def mask_set(self, i, key, value, source="ui"):
        m = self.mask(i)
        if m is None:
            return
        d = m.setdefault("params", {})
        if d.get(key, 0.0) == value:
            return
        d[key] = value
        self.changed.emit(source)

    def mask_add(self, kind, source="ui"):
        self.push()
        self.p.setdefault("masks", []).append(gc.new_group(kind))
        self.changed.emit(source)
        return len(self.p["masks"]) - 1

    def component_add(self, i, kind, source="ui"):
        m = self.mask(i)
        if m is None:
            return -1
        self.push()
        if "components" not in m:          # 老式扁平蒙版先升级成单组件组
            g = gc.as_group(m)
            m.clear()
            m.update(g)
        m.setdefault("components", []).append(gc.new_component(kind))
        self.changed.emit(source)
        return len(m["components"]) - 1

    def component_remove(self, i, j, source="ui"):
        m = self.mask(i)
        cs = (m or {}).get("components") or ()
        if not (0 <= j < len(cs)) or len(cs) < 2:
            return
        self.push()
        del cs[j]
        self.changed.emit(source)

    def mask_remove(self, i, source="ui"):
        ms = self.p.get("masks") or ()
        if not (0 <= i < len(ms)):
            return
        self.push()
        del self.p["masks"][i]
        self.changed.emit(source)

    def mask_move(self, i, u, v, end=False, j=0, source="ui"):
        """拖动几何：end=False 挪起点/中心，True 挪终点/边缘。"""
        m = self.comp(i, j)
        if m is None:
            return
        u = float(min(max(u, 0.0), 1.0))
        v = float(min(max(v, 0.0), 1.0))
        if end:
            if (m["x2"], m["y2"]) == (u, v):
                return
            m["x2"], m["y2"] = u, v
        else:
            if (m["x1"], m["y1"]) == (u, v):
                return
            m["x1"], m["y1"] = u, v
        self.changed.emit(source)

    def mask_shift(self, i, du, dv, j=0, source="ui"):
        """整体平移：起点与终点同时走，用来挪蒙版位置而不改方向/大小。"""
        m = self.comp(i, j)
        if m is None:
            return
        m["x1"] = float(min(max(m["x1"] + du, 0.0), 1.0))
        m["y1"] = float(min(max(m["y1"] + dv, 0.0), 1.0))
        m["x2"] = float(min(max(m["x2"] + du, 0.0), 1.0))
        m["y2"] = float(min(max(m["y2"] + dv, 0.0), 1.0))
        self.changed.emit(source)

    def mask_toggle(self, i, source="ui"):
        m = self.mask(i)
        if m is None:
            return
        self.push()
        m["invert"] = 0 if m.get("invert") else 1
        self.changed.emit(source)

    def component_toggle(self, i, j, source="ui"):
        m = self.comp(i, j)
        if m is None:
            return
        self.push()
        m["invert"] = 0 if m.get("invert") else 1
        self.changed.emit(source)

    def component_blend(self, i, j, blend, source="ui"):
        m = self.comp(i, j)
        if m is None or blend not in ("add", "subtract", "intersect"):
            return
        if m.get("blend", "add") == blend:
            return
        self.push()
        m["blend"] = blend
        self.changed.emit(source)

    def mask_reset(self, i, source="ui"):
        """把当前组的增量全部归零（几何保留）。"""
        m = self.mask(i)
        if m is None or not (m.get("params") or {}):
            return
        self.push()
        m["params"] = {}
        self.changed.emit(source)

    # ---- 亮度范围（组级；组件级仍可由旁车/脚本设置） ----
    RANGE_NEW = {"lum_min": 0.0, "lum_max": 0.5, "lum_feather": 0.1}

    def mask_range(self, i):
        m = self.mask(i) or {}
        return {**self.RANGE_NEW, **(m.get("range") or {})}

    def mask_range_on(self, i, on, source="ui"):
        m = self.mask(i)
        if m is None:
            return
        rm = m.setdefault("range", {})
        if bool(rm.get("lum_on")) == bool(on):
            return
        self.push()
        for k, v in self.RANGE_NEW.items():
            rm.setdefault(k, v)
        rm["lum_on"] = 1 if on else 0
        self.changed.emit(source)

    def range_value(self, i, key):
        return float(self.mask_range(i).get(key, 0.0)) * 100.0

    def range_set(self, i, key, value, source="ui"):
        m = self.mask(i)
        if m is None:
            return
        rm = m.setdefault("range", {})
        for k, v in self.RANGE_NEW.items():
            rm.setdefault(k, v)
        v = min(max(float(value), 0.0), 100.0) / 100.0
        if key == "lum_min":
            v = min(v, float(rm["lum_max"]))
        elif key == "lum_max":
            v = max(v, float(rm["lum_min"]))
        else:
            v = max(v, 0.001)
        if abs(float(rm.get(key, -1.0)) - v) < 1e-9:
            return
        rm[key] = v
        self.changed.emit(source)

    def mask_attr(self, i, name, default=0.0, j=None):
        m = self.mask(i)
        if m is None:
            return default
        if j is not None or name in self.GEOM_ATTRS:
            c = self.comp(i, 0 if j is None else j)
            return (c or {}).get(name, default)
        return m.get(name, default)

    def mask_set_attr(self, i, name, value, source="ui", j=None):
        """几何/强度属性：绝对值，直接写在组或组件字典上。"""
        m = self.mask(i)
        if m is None:
            return
        geom = j is not None or name in self.GEOM_ATTRS
        target = self.comp(i, 0 if j is None else j) if geom else m
        if target is None or target.get(name) == value:
            return
        target[name] = value
        self.changed.emit(source)

    # ---- 历史 ----
    def begin_edit(self):
        if self._pending is None:
            self._pending = copy.deepcopy(self.p)

    def end_edit(self, source="ui"):
        if self._pending is None:
            return
        if self._pending != self.p:
            self._undo.append(self._pending)
            if len(self._undo) > 100:
                self._undo.pop(0)
            self._redo.clear()
            self.history_changed.emit()
        self._pending = None
        self.changed.emit(source)

    def cancel_edit(self):
        if self._pending is not None and self._pending != self.p:
            self.p = self._pending
            self.changed.emit("undo")
        self._pending = None

    def push(self):
        """把当前状态记为一步（用于曲线加点、预设等离散操作前的快照）。"""
        self._undo.append(copy.deepcopy(self.p))
        if len(self._undo) > 100:
            self._undo.pop(0)
        self._redo.clear()
        self.history_changed.emit()

    def undo(self):
        if not self._undo:
            return False
        self._redo.append(copy.deepcopy(self.p))
        self.p = self._undo.pop()
        self.changed.emit("undo")
        self.history_changed.emit()
        return True

    def redo(self):
        if not self._redo:
            return False
        self._undo.append(copy.deepcopy(self.p))
        self.p = self._redo.pop()
        self.changed.emit("undo")
        self.history_changed.emit()
        return True

    def can_undo(self):
        return bool(self._undo)

    def can_redo(self):
        return bool(self._redo)

    # ---- 批量操作 ----
    def reset_keys(self, keys):
        self.push()
        d = gc.defaults()
        for k in keys:
            self.p[k] = copy.deepcopy(d[k])
        self.changed.emit("reset")

    def reset_all(self):
        self.push()
        self.p = gc.defaults()
        self.changed.emit("reset")

    def load_preset(self, name):
        self.push()
        self.p = gc.preset(name)
        self.changed.emit("preset")

    def merge(self, values, source="auto"):
        """一次写入多项（自动校正）：只记一步历史、只触发一次重渲染。"""
        self.push()
        for k, v in values.items():
            self.p[k] = copy.deepcopy(v) if isinstance(v, list) else v
        self.changed.emit(source)

    def snapshot(self):
        return copy.deepcopy(self.p)

    def restore(self, p):
        self.p = copy.deepcopy(p)
        self.changed.emit("undo")

    def load(self, p):
        """切换项目/恢复旁车：替换参数并清空历史，撤销不能跨项目。"""
        self.p = copy.deepcopy(p)
        self._undo.clear()
        self._redo.clear()
        self._pending = None
        self.history_changed.emit()
        self.changed.emit("load")
