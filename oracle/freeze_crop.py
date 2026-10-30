# -*- coding: utf-8 -*-
"""冻结 Python 选区几何（lapse/crop.py）的输入/输出，作为 Rust 重写的回归标尺。

只读开发树、只写 oracle/golden/crop.json，跑两遍必须逐字节一致。

用法（仓库根目录，用开发树的解释器）：
    ../pythonProject4/.venv/Scripts/python.exe oracle/freeze_crop.py
"""
import argparse
import json
import random
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_DEV = ROOT.parent / "pythonProject4"

FRAMES = [(640, 480), (1024, 768), (999, 667), (1920, 1080), (4001, 2667), (8, 6)]
ASPECTS = [None, (16, 9), (3, 2), (4, 3), (1, 1), (9, 16), (12, 5)]
HANDLES = ["nw", "n", "ne", "e", "se", "s", "sw", "w"]
# 只落在 .25/.75 上：Python 的 round() 对正好 .5 走"四舍六入成偶"，Rust 走远离零，
# 那种点上一边会差一个像素，拿来做基准就是在比两条不同的规则。
FRACS = [0.0, 0.25, 0.75]


def point(rng, w, h):
    return (rng.randrange(0, w + 1) + rng.choice(FRACS),
            rng.randrange(0, h + 1) + rng.choice(FRACS))


def sel_rect(crop, rng, w, h):
    x0, y0 = point(rng, w, h)
    x1, y1 = point(rng, w, h)
    return crop.norm_rect(x0, y0, x1, y1, w, h)


def build(crop, rng):
    cases = {}

    cases["norm"] = []
    for _ in range(40):
        fw, fh = rng.choice(FRAMES)
        x0, y0 = point(rng, fw, fh)
        x1, y1 = point(rng, fw, fh)
        cases["norm"].append({"x0": x0, "y0": y0, "x1": x1, "y1": y1, "frame": [fw, fh],
                              "out": crop.norm_rect(x0, y0, x1, y1, fw, fh)})

    cases["center"] = []
    for fw, fh in FRAMES:
        for asp in ASPECTS:
            cases["center"].append({"frame": [fw, fh], "aspect": asp,
                                    "out": crop.center_rect(fw, fh, asp)})

    cases["refit"] = []
    for _ in range(20):
        fw, fh = rng.choice(FRAMES)
        sel, asp = sel_rect(crop, rng, fw, fh), rng.choice(ASPECTS)
        cases["refit"].append({"sel": sel, "frame": [fw, fh], "aspect": asp,
                               "out": crop.refit_rect(sel, fw, fh, asp)})

    cases["move"] = []
    for _ in range(20):
        fw, fh = rng.choice(FRAMES)
        sel = sel_rect(crop, rng, fw, fh)
        px, py = point(rng, fw, fh)
        cases["move"].append({"sel": sel, "frame": [fw, fh], "at": [px, py],
                              "out": crop.move_rect(sel, px, py, fw, fh)})

    cases["draw"] = []
    for _ in range(30):
        fw, fh = rng.choice(FRAMES)
        ax, ay = point(rng, fw, fh)
        px, py = point(rng, fw, fh)
        asp = rng.choice(ASPECTS)
        cases["draw"].append({"anchor": [ax, ay], "at": [px, py], "frame": [fw, fh], "aspect": asp,
                              "out": crop.draw_rect((ax, ay), px, py, fw, fh, asp)})

    cases["resize"] = []
    for _ in range(80):
        fw, fh = rng.choice(FRAMES)
        sel, handle = sel_rect(crop, rng, fw, fh), rng.choice(HANDLES)
        px, py = point(rng, fw, fh)
        asp = rng.choice(ASPECTS)
        cases["resize"].append({"handle": handle, "sel": sel, "at": [px, py], "frame": [fw, fh],
                                "aspect": asp,
                                "out": crop.resize_rect(handle, sel, px, py, fw, fh, asp)})

    cases["nudge"] = []
    for _ in range(30):
        fw, fh = rng.choice(FRAMES)
        sel = sel_rect(crop, rng, fw, fh)
        dx, dy = rng.randrange(-2, 3), rng.randrange(-2, 3)
        step = rng.choice([1, 2, 10])
        cases["nudge"].append({"sel": sel, "d": [dx, dy], "step": step, "frame": [fw, fh],
                               "out": crop.nudge_rect(sel, dx, dy, step, fw, fh)})

    cases["hit"] = []
    for _ in range(40):
        fw, fh = rng.choice(FRAMES)
        sel = sel_rect(crop, rng, fw, fh)
        # 命中检测本来在屏幕坐标上做，这里把选区原样当屏幕矩形，只验规则本身。
        box = [sel[0], sel[1], sel[0] + sel[2], sel[1] + sel[3]]
        for px, py in (point(rng, fw, fh), (box[0], box[1]), (box[2], box[3]),
                       (box[2] + 6.0, box[3] + 6.0), (box[0] + 2.0, box[1] + 2.0)):
            cases["hit"].append({"rect": box, "at": [px, py], "out": crop.hit_test(box, px, py)})

    return cases


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dev-tree", default=str(DEFAULT_DEV))
    parser.add_argument("--seed", type=int, default=42)
    args = parser.parse_args()

    sys.path.insert(0, args.dev_tree)
    from lapse import crop

    cases = build(crop, random.Random(args.seed))
    doc = {"generator": "oracle/freeze_crop.py", "seed": args.seed, "cases": cases}
    dst = ROOT / "oracle" / "golden" / "crop.json"
    dst.write_text(json.dumps(doc, ensure_ascii=False, indent=1, sort_keys=True) + "\n",
                   encoding="utf-8")
    print(f"==> 写入 {dst}：{sum(len(v) for v in cases.values())} 条用例")


if __name__ == "__main__":
    main()
