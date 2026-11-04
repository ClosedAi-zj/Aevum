#!/usr/bin/env python3
"""从可变字体里切一份「静态字重 + 只留界面用得到的字」的子集字体。

为什么需要它：界面只给分区标题与次级小标题用粗体，一份完整的 Noto Sans SC
是 10.6 MB，为几十个字付这个体积不值。做法是标准的 fontTools 两条流水线——
`varLib.instancer` 把 `wght` 轴钉死在目标值（产出静态字重），`subset` 只留
源码里出现过的字。

字集是**扫出来的**，不是手写的：把 `crates/aevum-ui/src` 下所有 `.rs` 文本里
落在目标 Unicode 区段的字符全收进来（注释里的字也收，宁可是超集）。所以新加
一句中文标题、重跑这个脚本，子集自动跟上；忘了重跑也不会变豆腐块——字族列表里
粗体排在常规体前面，缺字自动落到常规体那一份（见 `theme::set_fonts`）。

可复现：`SOURCE_DATE_EPOCH` 钉住 `head.created/modified`，同一份源码与同一份
可变字体跑两遍得到逐字节相同的产物。

用法：
    python tools/font_subset.py            # 重生成 assets/fonts 下两份子集
    python tools/font_subset.py --check     # 只比对，不写盘（CI 用）

Re-cut a static weight subset from a variable font, keeping only the characters
the UI actually uses.

Why: bold is used for section headings only, and a full Noto Sans SC weighs
10.6 MB -- not worth paying for a few dozen glyphs. Two standard fontTools
passes: `varLib.instancer` pins the `wght` axis (producing a static weight),
then `subset` keeps only characters found in the source.

The character set is *scanned*, not hand-written: every character inside the
target Unicode ranges across all `.rs` text under `crates/aevum-ui/src` is
collected (including comments -- a superset is fine). Add a Chinese heading,
re-run this script, and the subset follows. Forgetting to re-run does not
produce tofu: the bold family lists the regular family behind it, so missing
glyphs fall back to regular (see `theme::set_fonts`).

Reproducible: `SOURCE_DATE_EPOCH` pins `head.created/modified`, so the same
source and the same variable font produce byte-identical output.

Usage:
    python tools/font_subset.py            # regenerate the two subsets in assets/fonts
    python tools/font_subset.py --check     # compare only, write nothing (for CI)
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

ROOT = Path(__file__).resolve().parent.parent
SCAN_DIRS = [ROOT / "crates" / "aevum-ui" / "src", ROOT / "apps" / "aevum-studio" / "src"]

# 收字区段：ASCII、拉丁补充、常用标点、中日韩标点、CJK 统一表意、全角形式。
#
# Ranges to collect: ASCII, Latin supplement, general punctuation, CJK
# punctuation, CJK unified ideographs, fullwidth forms.
RANGES = [
    (0x0020, 0x007E),
    (0x00A0, 0x00FF),
    (0x2000, 0x206F),
    (0x2100, 0x214F),
    (0x2190, 0x21FF),
    (0x2200, 0x22FF),
    (0x2460, 0x24FF),
    (0x25A0, 0x25FF),
    (0x3000, 0x303F),
    (0x4E00, 0x9FFF),
    (0xFF00, 0xFFEF),
]

JOBS = [
    # (可变字体, 轴值, 输出, 是否只留扫出来的字)
    ("Inter-Variable.ttf", 700.0, "Inter-700-Subset.ttf", True),
    ("NotoSansSC-Variable.ttf", 700.0, "NotoSansSC-700-Subset.ttf", True),
]


def scanned_chars() -> set[str]:
    """源码里出现过的、落在 RANGES 内的字符。"""
    chars: set[str] = set()
    for base in SCAN_DIRS:
        for path in sorted(base.rglob("*.rs")):
            text = path.read_text(encoding="utf-8", errors="ignore")
            for ch in text:
                cp = ord(ch)
                for lo, hi in RANGES:
                    if lo <= cp <= hi:
                        chars.add(ch)
                        break
    return chars


def build(vari: Path, weight: float, out: Path, chars: set[str], check: bool) -> None:
    font = TTFont(vari)
    axes = {a.axisTag: (a.minValue, a.defaultValue, a.maxValue) for a in font["fvar"].axes}
    lo, _, hi = axes["wght"]
    if not lo <= weight <= hi:
        raise SystemExit(f"{vari.name}: wght {weight} 不在 [{lo}, {hi}] 内")
    static = instancer.instantiateVariableFont(font, {"wght": weight}, inplace=False)

    tmp = out.with_suffix(".tmp.ttf")
    static.save(tmp)
    static.close()

    opts = subset.Options()
    opts.layout_features = ["*"]
    opts.name_IDs = ["*"]
    opts.name_legacy = True
    opts.notdef_outline = True
    opts.recalc_bounds = True
    opts.drop_tables += ["DSIG"]
    # 保留 hinting 与所有 OpenType 特性：13px 的小字去掉 hinting 会发糊。
    #
    # Keep hinting and every OpenType feature: hinting-free text looks soft at 13 px.
    opts.hinting = True

    font2 = subset.load_font(str(tmp), opts)
    subsetter = subset.Subsetter(options=opts)
    subsetter.populate(unicodes=[ord(c) for c in chars])
    subsetter.subset(font2)
    subset.save_font(font2, str(tmp), opts)
    font2.close()

    os.replace(tmp, out)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true", help="只比对产物是否已是最新")
    args = ap.parse_args()

    # head.created / modified 的时间戳来源：钉住才能逐字节复现。
    #
    # Source of head.created / modified: pinning it is what makes the output
    # byte-reproducible.
    os.environ.setdefault("SOURCE_DATE_EPOCH", "1767225600")  # 2026-01-01T00:00:00Z

    chars = scanned_chars()
    cjk = sum(1 for c in chars if ord(c) > 0x2E7F)
    print(f"扫到 {len(chars)} 个字符（其中 CJK/全角 {cjk} 个）")

    for name, weight, out_name, use_chars in JOBS:
        vari = ROOT / "assets" / "fonts" / name
        out = ROOT / "assets" / "fonts" / out_name
        before = out.stat().st_size if out.exists() else -1
        if args.check:
            tmpdir = Path(os.environ.get("TMP", "/tmp")) / "aevum-font-check"
            tmpdir.mkdir(parents=True, exist_ok=True)
            probe = tmpdir / out_name
            build(vari, weight, probe, chars, check=False)
            if not out.exists() or probe.read_bytes() != out.read_bytes():
                print(f"  {out_name}: 与源码不同步（重跑 tools/font_subset.py）")
                return 1
            print(f"  {out_name}: 一致（{before:,} B）")
            continue
        build(vari, weight, out, chars, check=False)
        print(f"  {out_name}: {before:,} B -> {out.stat().st_size:,} B")
    return 0


if __name__ == "__main__":
    sys.exit(main())
