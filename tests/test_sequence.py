# -*- coding: utf-8 -*-
"""序列关键帧插值的纯算法回归：标量、色相、曲线、列表、蒙版、关键帧表。

用法： ./.venv/Scripts/python.exe tests/test_sequence.py
"""
import copy
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "acceptance"))

from reference import grade as gc       # noqa: E402
from reference import sequence as seq   # noqa: E402

FAILS = []


def check(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}"
          + (f"  {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def near(a, b, tol=1e-6):
    return abs(float(a) - float(b)) <= tol


def test_scalars():
    a, b = gc.defaults(), gc.defaults()
    a["exposure"], b["exposure"] = -1.0, 3.0
    a["contrast"], b["contrast"] = 0.0, 100.0
    before_a, before_b = copy.deepcopy(a), copy.deepcopy(b)
    mid = seq.interpolate(a, b, 0.25)
    check("标量线性插值", near(mid["exposure"], 0.0)
          and near(mid["contrast"], 25.0),
          f"{mid['exposure']} {mid['contrast']}")
    check("端点原样返回",
          seq.interpolate(a, b, 0.0) == a and seq.interpolate(a, b, 1.0) == b)
    check("插值不修改输入", a == before_a and b == before_b)
    mid["exposure"] = 99.0
    check("返回的是深拷贝", near(a["exposure"], -1.0))


def test_hue():
    a, b = gc.defaults(), gc.defaults()
    a["grade_sh_h"], b["grade_sh_h"] = 350.0, 10.0
    m1 = seq.interpolate(a, b, 0.5)["grade_sh_h"]
    m2 = seq.interpolate(a, b, 0.25)["grade_sh_h"]
    m3 = seq.interpolate(a, b, 0.75)["grade_sh_h"]
    check("色相走最短弧（350→10）", near(m1, 0.0) and near(m2, 355.0)
          and near(m3, 5.0), f"{m1} {m2} {m3}")
    a["grade_hi_h"], b["grade_hi_h"] = 10.0, 350.0
    check("反方向也走最短弧",
          near(seq.interpolate(a, b, 0.5)["grade_hi_h"], 0.0),
          str(seq.interpolate(a, b, 0.5)["grade_hi_h"]))
    a["grade_sh_h"], b["grade_sh_h"] = 30.0, 120.0
    check("普通色相直接插值",
          near(seq.interpolate(a, b, 0.5)["grade_sh_h"], 75.0))
    a["grade_global_h"], b["grade_global_h"] = 350.0, 10.0
    check("全局色相也走最短弧",
          near(seq.interpolate(a, b, 0.5)["grade_global_h"], 0.0))


def test_curve():
    a, b = gc.defaults(), gc.defaults()
    a["curve_rgb"] = [[0, 0], [255, 255]]
    b["curve_rgb"] = [[0, 0], [64, 20], [128, 180], [255, 255]]
    mid = seq.interpolate(a, b, 0.5)["curve_rgb"]
    xs = [p[0] for p in mid]
    check("曲线重采样成固定点数", len(mid) == seq.CURVE_SAMPLES,
          str(len(mid)))
    check("曲线端点保持一致", mid[0] == [0, 0] and mid[-1] == [255, 255],
          f"{mid[0]} {mid[-1]}")
    check("曲线横坐标递增", all(x1 < x2 for x1, x2 in zip(xs, xs[1:])),
          str(xs[:4]))
    la = gc.pchip_lut(a["curve_rgb"], 256)
    lb = gc.pchip_lut(b["curve_rgb"], 256)
    px = min(mid, key=lambda q: abs(q[0] - 128))
    want = (float(la[128]) + float(lb[128])) / 2.0
    check("曲线纵值按 LUT 平均", abs(px[1] - want) <= 1.0,
          f"{px} vs {want:.1f}")
    same = seq.interpolate(a, copy.deepcopy(a), 0.5)["curve_rgb"]
    check("相同曲线不改形状", same == a["curve_rgb"], str(same))


def test_lists():
    a, b = gc.defaults(), gc.defaults()
    a["hsl_s"] = [-50.0] + [0.0] * 7
    b["hsl_s"] = [50.0] + [20.0] * 7
    mid = seq.interpolate(a, b, 0.5)
    check("HSL 逐元素插值", len(mid["hsl_s"]) == 8
          and near(mid["hsl_s"][0], 0.0) and near(mid["hsl_s"][3], 10.0),
          str(mid["hsl_s"][:4]))
    check("列表不丢长度", all(len(mid[k]) == 8 for k in
                             ("hsl_h", "hsl_s", "hsl_l", "mono_h")))


def test_masks():
    a, b = gc.defaults(), gc.defaults()
    ma = gc.new_mask("linear")
    ma["id"] = "aaaa"
    ma["x2"] = 0.6
    ma["params"] = {"exposure": 1.0}
    mb = gc.new_mask("linear")
    mb["id"] = "aaaa"
    mb["x2"] = 0.8
    mb["params"] = {"exposure": 3.0}
    a["masks"], b["masks"] = [ma], [mb]
    mid = seq.interpolate(a, b, 0.5)
    check("蒙版按稳定 id 配对", len(mid["masks"]) == 1
          and mid["masks"][0]["id"] == "aaaa"
          and near(mid["masks"][0]["x2"], 0.7)
          and near(mid["masks"][0]["params"]["exposure"], 2.0),
          str(mid["masks"][0]))
    a_inv = copy.deepcopy(a)
    b_inv = copy.deepcopy(b)
    b_inv["masks"][0]["invert"] = 1
    check("反转按中点切换",
          seq.interpolate(a_inv, b_inv, 0.4)["masks"][0]["invert"] == 0
          and seq.interpolate(a_inv, b_inv, 0.6)["masks"][0]["invert"] == 1)

    only_a = seq.interpolate(a, gc.defaults(), 0.25)["masks"]
    only_b = seq.interpolate(gc.defaults(), b, 0.25)["masks"]
    check("单侧蒙版淡出", len(only_a) == 1
          and near(only_a[0]["amount"], 75.0), str(only_a))
    check("单侧蒙版淡入", len(only_b) == 1
          and near(only_b[0]["amount"], 25.0), str(only_b))

    no_id_a, no_id_b = gc.defaults(), gc.defaults()
    m1 = gc.new_mask("radial")
    m2 = gc.new_mask("radial")
    m1.pop("id")
    m2.pop("id")
    m1["params"], m2["params"] = {"exposure": 0.0}, {"exposure": 4.0}
    no_id_a["masks"], no_id_b["masks"] = [m1], [m2]
    mid = seq.interpolate(no_id_a, no_id_b, 0.5)["masks"]
    check("旧蒙版无 id 时按序号兜底", len(mid) == 1
          and near(mid[0]["params"]["exposure"], 2.0),
          str(mid))


def test_table():
    p2 = gc.defaults()
    p2["exposure"] = 2.0
    p5 = gc.defaults()
    p5["exposure"] = 5.0
    raw = [{"index": 5, "params": p5},
           {"index": 2, "params": gc.defaults()},
           {"index": 2, "params": p2},
           {"index": -1, "params": p5},
           {"index": 99, "params": p5},
           "坏项"]
    norm = seq.normalize(raw, count=10)
    check("关键帧排序去重裁越界",
          [k["index"] for k in norm] == [2, 5]
          and near(norm[0]["params"]["exposure"], 2.0),
          str([k["index"] for k in norm]))
    up = seq.upsert(norm, 3, p5)
    check("插入保持有序", seq.indices(up) == [2, 3, 5], str(seq.indices(up)))
    up2 = seq.upsert(up, 3, p2)
    check("同帧覆盖不重复", len(up2) == 3
          and near(seq.at(up2, 3)["params"]["exposure"], 2.0))
    rm = seq.remove(up2, 3)
    check("删除只删指定帧", seq.indices(rm) == [2, 5], str(seq.indices(rm)))
    check("空表取不到", seq.at([], 0) is None and not seq.indices([]))

    kfs = [{"index": 0, "params": gc.defaults()},
           {"index": 10, "params": p5}]
    check("区间内线性取参数",
          near(seq.params_at(gc.defaults(), kfs, 5)["exposure"], 2.5),
          str(seq.params_at(gc.defaults(), kfs, 5)["exposure"]))
    check("范围外夹到最近关键帧",
          near(seq.params_at(gc.defaults(), kfs, -3)["exposure"], 0.0)
          and near(seq.params_at(gc.defaults(), kfs, 99)["exposure"], 5.0))
    one = [{"index": 4, "params": p2}]
    check("只有一个关键帧时全段恒定",
          near(seq.params_at(gc.defaults(), one, 0)["exposure"], 2.0)
          and near(seq.params_at(gc.defaults(), one, 99)["exposure"], 2.0))
    base = gc.defaults()
    check("没有关键帧时返回基准",
          seq.params_at(base, [], 7) is base)


def test_track():
    """逐帧曝光单值轨：必须和 params_at(...)["exposure"] 逐帧一致。

    画"校正后"亮度曲线要每帧的曝光，走 params_at 得整套深拷贝，几千帧太贵，
    所以有这条便宜路径——它一旦和权威路径算出不一样的数，界面上的线就开始骗人。
    """
    p5 = gc.defaults()
    p5["exposure"] = 5.0
    p2 = gc.defaults()
    p2["exposure"] = -1.5
    base = gc.defaults()
    kfs = [{"index": 0, "params": base},
           {"index": 6, "params": p5},
           {"index": 11, "params": p2}]
    track = seq.exposure_track(base, kfs, 12)
    worst = max(abs(track[i] - seq.params_at(base, kfs, i)["exposure"])
                for i in range(12))
    check("曝光轨与整套插值逐帧一致", worst < 1e-9, f"最大偏差 {worst:.2e}")
    check("曝光轨形状（坡起坡落）", len(track) == 12 and near(track[3], 2.5)
          and near(track[6], 5.0) and near(track[9], 1.1),
          str([round(v, 2) for v in track]))
    check("没有关键帧时每帧同值",
          seq.exposure_track(p5, [], 4) == [5.0, 5.0, 5.0, 5.0])
    one = [{"index": 4, "params": p5}]
    check("单关键帧前后夹到最近值",
          seq.exposure_track(base, one, 8) == [5.0] * 8,
          str(seq.exposure_track(base, one, 8)))
    check("缺 exposure 按 0 算",
          seq.exposure_track(base, [{"index": 2, "params": {}}], 5)
          == [0.0] * 5)
    check("坏输入不炸",
          seq.exposure_track(base, [{"index": "x", "params": p5}], 3)
          == [0.0, 0.0, 0.0]
          and seq.exposure_track(base, kfs, 0) == []
          and seq.exposure_track(base, kfs, None) == [])
    # 拖动过程中曲线每帧都要重算：这条路径得比"整套参数深拷贝"便宜一个量级，
    # 否则几千帧的序列每挪一下就要卡一下
    big = [{"index": 0, "params": base}, {"index": 1999, "params": p5}]
    t0 = time.perf_counter()
    for _ in range(10):
        seq.exposure_track(base, big, 2000)
    track_ms = (time.perf_counter() - t0) / 10 / 2000 * 1000
    t0 = time.perf_counter()
    for i in range(2000):
        seq.params_at(base, big, i)
    full_ms = (time.perf_counter() - t0) / 2000 * 1000
    check("单值轨比整套插值便宜一个量级",
          track_ms * 8 < full_ms,
          f"{track_ms * 1000:.2f} µs/帧 vs {full_ms * 1000:.2f} µs/帧")


def main():
    test_scalars()
    test_hue()
    test_curve()
    test_lists()
    test_masks()
    test_table()
    test_track()
    print("\n" + ("全部通过" if not FAILS else f"失败 {len(FAILS)}: {FAILS}"))
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
