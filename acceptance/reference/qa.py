# -*- coding: utf-8 -*-
"""序列体检：从逐帧亮度测量里挑出坏帧 / 黑帧 / 虚焦 / 离群 / 重复 / 缺口。

输入是 acceptance/reference/luma.py 的测量结果 (mean, median, center, sharp)，本模块不读图、
不起线程，纯数据；GUI 只负责把结果画成一张可勾选的表。

为什么不用绝对阈值：
  * sharp 是 0~255 灰度上的拉普拉斯方差，量纲随分辨率、降噪和构图变化，
    写死 0.004 之类只对归一化方差成立。
  * 延时序列的亮度本身就在变（日转夜能差十几档），拿全序列中位数算 z 分数
    会把日出和日落两头全都报成离群 —— 误报一多，用户就不看这张表了。

判定办法：逐帧跟**左边**和**右边**各若干帧的中位数比，只有两侧同向偏离才算离群。
亮度渐变时左偏正、右偏负（自己正好落在趋势线上），台阶处一侧偏离≈0，两种都归零；
单帧被碰歪、虚焦、曝光飞了才会两侧同向。门槛 = max(绝对下限, sig × 序列自身的波动中位数)，
所以噪声大的序列自动放宽，干净序列能抓到 5% 的亮度跳变。
"""
import os
import re

import numpy as np

KINDS = ("read", "black", "blur", "dup", "luma_out", "gap")
LABELS = {"read": "读不出来", "black": "几乎全黑", "blur": "疑似虚焦",
          "dup": "疑似重复", "luma_out": "亮度离群", "gap": "序列缺口"}

MIN_SAMPLES = 8           # 少于这么多可用帧就不做离群判定（统计没意义）
MIN_SIDE = 3              # 左右各至少几帧才构成可信的局部中位数
# 黑帧/坏帧的虚焦、离群表现只是同一件事的副作用，不重复列
SELF_EXPLAINED = ("blur", "luma_out")
# 曝光飞了的帧对比度也塌了，清晰度结论并入亮度离群
SECONDARY_FOR = ("blur",)
_NUM = re.compile(r"^(.*?)(\d+)$")


def label(kind):
    return LABELS.get(kind, kind)


def excludable(kind):
    """缺口对应的文件不存在，无从排除，只能在表里告知。"""
    return kind != "gap"


def _values(luma, slot):
    out = np.full(len(luma), np.nan, dtype=np.float64)
    for i, v in enumerate(luma):
        if v is not None and len(v) > slot:
            out[i] = float(v[slot])
    return out


def _resid(y, half, min_side=MIN_SIDE):
    """log 序列 y -> (同向偏离量, 方向, 是否判得到)。

    左偏离 = y[i] - 左侧窗中位数，右偏离 = y[i] - 右侧窗中位数。渐变时两者
    异号、台阶时一侧接近 0，都归零；离群帧两侧同号，取较小者当偏离量，所以
    要求异常既突出又孤立。够不到 min_side 个邻居的帧头帧尾判不到（dev 记 0）。
    """
    n = len(y)
    dev = np.zeros(n, dtype=np.float64)
    sgn = np.zeros(n, dtype=np.float64)
    judged = np.zeros(n, dtype=bool)
    for i in range(n):
        if not np.isfinite(y[i]):
            continue
        L = [y[j] for j in range(max(0, i - half), i) if np.isfinite(y[j])]
        R = [y[j] for j in range(i + 1, min(n, i + half + 1))
             if np.isfinite(y[j])]
        if len(L) < min_side or len(R) < min_side:
            continue
        judged[i] = True
        rl = float(y[i] - np.median(L))
        rr = float(y[i] - np.median(R))
        if rl * rr <= 0:
            continue
        dev[i] = min(abs(rl), abs(rr))
        sgn[i] = 1.0 if rl > 0 else -1.0
    return dev, sgn, judged


def _threshold(dev, judged, sig, floor):
    """门槛 = max(绝对下限, sig × 本序列偏离中位数)。

    中位数把判不到/被趋势吃掉的那些帧（dev=0）也算进去，所以干净序列的门槛
    就等于下限，不会因为只有一两个离群帧把自己的统计量抬起来。
    """
    if int(judged.sum()) < MIN_SAMPLES:
        return np.inf, 0.0
    jitter = float(np.median(dev))
    return max(floor, sig * jitter), jitter


def _pct(r):
    """把「是邻居的几成」写成人话：糊到 1/250 时报成 0% 等于没说。"""
    if r >= 0.1:
        return "%.0f%%" % (100.0 * r)
    return "1/%.0f" % (1.0 / max(r, 1e-9))


def _sizes(files):
    out = []
    for p in files:
        try:
            out.append(os.path.getsize(p))
        except OSError:
            out.append(-1)
    return out


def _gaps(files):
    """同目录同前缀的编号缺口：[(该缺口后一帧的索引, 缺失个数, 文字)]。

    编号取文件名末尾的连续数字；一个目录里不足两个编号文件就不算序列。
    """
    by = {}
    for i, p in enumerate(files):
        m = _NUM.match(os.path.splitext(os.path.basename(p))[0])
        if not m:
            continue
        by.setdefault((os.path.normcase(os.path.dirname(p)), m.group(1)),
                      []).append((int(m.group(2)), i))
    out = []
    for key, items in by.items():
        if len(items) < 2:
            continue
        items.sort()
        present = {n for n, _i in items}
        lo, hi = items[0][0], items[-1][0]
        missing = sorted(set(range(lo, hi + 1)) - present)
        if not missing:
            continue
        runs, cur = [], [missing[0]]
        for n in missing[1:]:
            if n == cur[-1] + 1:
                cur.append(n)
            else:
                runs.append(cur)
                cur = [n]
        runs.append(cur)
        for r in runs:
            after = next((i for n, i in items if n > r[-1]), None)
            if after is None:
                continue
            span = str(r[0]) if len(r) == 1 else "%d–%d" % (r[0], r[-1])
            out.append((after, len(r),
                        "缺 %d 帧（编号 %s）：成片里会按上一帧停留" % (len(r), span)))
    return out


def audit(files, luma, *, sig=3.0, black=0.01, blur_sig=3.0, window=9,
          luma_floor=0.05, blur_floor=0.30, gaps=True, dup=True):
    """返回 [(帧号, 类型, 数值, 说明)]，按帧号排序；类型见 KINDS。

    luma 必须与 files 等长（reference.luma.measure_luma 的原样输出，读不出的帧为 None）。
    window 是左右各看几帧，sig / blur_sig 是相对本序列波动的倍数门槛，
    luma_floor / blur_floor 是对数域的绝对下限（0.05 ≈ 亮度 5%，0.30 ≈ 清晰度 26%）。
    """
    files = list(files)
    if len(luma) != len(files):
        raise ValueError("luma 与 files 长度不一致")
    n = len(files)
    found = {}

    def add(i, kind, val, msg):
        found.setdefault(i, []).append((i, kind, float(val), msg))

    means = _values(luma, 0)
    sharps = _values(luma, 3)

    for i, v in enumerate(luma):
        if v is None:
            add(i, "read", 0.0, "这一帧读不出来（文件损坏或不是图片）")
        elif means[i] < black:
            add(i, "black", means[i], "亮度只有 %.4f，可能是误拍或镜头盖" % means[i])

    half = max(MIN_SIDE, int(window))
    if n >= MIN_SAMPLES:
        dev, sgn, judged = _resid(np.log(np.maximum(sharps, 1e-6)), half)
        thr, _ = _threshold(dev, judged, blur_sig, blur_floor)
        for i in np.flatnonzero((dev > thr) & (sgn < 0)):
            i = int(i)
            add(i, "blur", float(sharps[i]),
                "清晰度只有左右邻居的 %s，大概虚焦或抖糊了"
                % _pct(float(np.exp(-dev[i]))))
        dev, sgn, judged = _resid(np.log(np.maximum(means, 1e-6)), half)
        thr, _ = _threshold(dev, judged, sig, luma_floor)
        for i in np.flatnonzero(dev > thr):
            i = int(i)
            add(i, "luma_out", float(means[i]),
                "亮度比左右邻居的中位数%s %.0f%%（%+.1f 档）"
                % ("高" if sgn[i] > 0 else "低",
                   100.0 * float(np.expm1(dev[i])),
                   float(sgn[i] * dev[i]) / float(np.log(2.0))))

    if dup and n >= 2:
        sizes = _sizes(files)
        for i in range(1, n):
            if luma[i] is None or luma[i - 1] is None:
                continue
            same = sizes[i] == sizes[i - 1] and sizes[i] > 0 and \
                all(abs(float(luma[i][s]) - float(luma[i - 1][s])) < 1e-9
                    for s in range(len(luma[i])))
            if same:
                add(i, "dup", float(means[i]),
                     "与上一帧的测量和文件大小完全一致，可能是快门没动作")

    for i, items in list(found.items()):
        kinds = {k for _x, k, _v, _m in items}
        drop = set()
        if kinds & {"read", "black"}:
            drop |= set(SELF_EXPLAINED)
        if "luma_out" in kinds:
            drop |= set(SECONDARY_FOR)
        if drop:
            found[i] = [x for x in items if x[1] not in drop]

    out = [x for items in found.values() for x in items]
    if gaps:
        for after, count, msg in _gaps(files):
            out.append((after, "gap", float(count), msg))
    return sorted(out, key=lambda r: (r[0], KINDS.index(r[1])))


def count_by_kind(findings):
    """{类型: 帧数}，给界面写「发现 3 处：黑帧 1 / 虚焦 1 …」。"""
    out = {}
    for _i, kind, _v, _m in findings:
        out[kind] = out.get(kind, 0) + 1
    return out
