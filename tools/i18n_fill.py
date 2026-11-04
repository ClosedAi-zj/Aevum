#!/usr/bin/env python3
"""把一批译文合并进 `crates/aevum-ui/src/i18n_en.rs`。

`i18n_extract.py` 负责**发现**键（扫源码得出「代码里用到了哪些中文」），本脚本负责
**填值**。两者分工的理由：键来自代码、值来自人，混在一个脚本里会让「跑一遍就把
人工译文冲掉」成为可能。

用法：

    python tools/i18n_fill.py pairs.json            # 预演，只报会改什么
    python tools/i18n_fill.py pairs.json --write    # 落盘
    python tools/i18n_fill.py pairs.json --write --lang ja   # 写进日语表

`pairs.json` 是 `[["中文原文", "English"], ...]`。重复键取最后一次出现；若同一个
键给了两个不同译文，直接报错——那说明这批数据本身有矛盾，不该由脚本静默挑一个。

写入后整表按键的码点重排（与 `i18n_extract.py --write` 同一口径），文件头注释原样
保留。**已有的键会被新值覆盖**：这是刻意的——改译文就是重新填一次，不必先删。
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import i18n_table


def main() -> int:
    argv = sys.argv[1:]
    write = "--write" in argv
    lang = i18n_table.DEFAULT_LANG
    rest: list[str] = []
    it = iter(argv)
    for a in it:
        if a == "--write":
            continue
        if a == "--lang":
            lang = next(it, lang)
            continue
        if a.startswith("--lang="):
            lang = a.split("=", 1)[1]
            continue
        rest.append(a)
    if len(rest) != 1:
        print(__doc__)
        return 2

    pairs = json.loads(Path(rest[0]).read_text(encoding="utf-8"))
    incoming: dict[str, str] = {}
    neutral = 0
    for item in pairs:
        if not (isinstance(item, list) and len(item) == 2):
            raise SystemExit(f"i18n_fill: {item!r} 不是 [源文, 译文] 二元组")
        zh, en = item
        if not isinstance(zh, str) or not isinstance(en, str):
            raise SystemExit(f"i18n_fill: {item!r} 两侧都必须是字符串")
        if not zh.strip():
            raise SystemExit("i18n_fill: 中文键不能为空")
        if not en.strip():
            raise SystemExit(f"i18n_fill: {zh!r} 的译文是空的——没译好就别写进来")
        # 译文与原文相同（`HSL`、`confirm` 这类缩写与参数名）**照样入表**。
        # 曾经这里跳过、写盘时还把历史遗留的 a→a 一并删掉，理由是「查不到就回退原文，
        # 效果一样」——运行时确实一样，但 `every_t_literal_has_a_row` 判据要求每个
        # `t!` 字面量都有行，删掉它当场变红（2026-11-05 实测：删掉 `HSL` 之后那条判据
        # 报「1 处回退成中文」）。两处的口径应当是同一个：**表里有行**才叫译过。
        if zh == en:
            neutral += 1
        if zh in incoming and incoming[zh] != en:
            raise SystemExit(f"i18n_fill: {zh!r} 给了两个不同译文：\n  {incoming[zh]!r}\n  {en!r}")
        incoming[zh] = en

    _head, existing = i18n_table.load(lang)

    added = [k for k in incoming if k not in existing]
    updated = [k for k in incoming if k in existing and existing[k] != incoming[k]]
    same = len(incoming) - len(added) - len(updated)
    merged = {**existing, **incoming}
    # 不再清 a→a：见上面那段——「语言中立」不等于「不必有行」。
    pruned: list[str] = []

    print(
        f"i18n_fill: 现有 {len(existing)} 条 · 新增 {len(added)} · 改译 {len(updated)} · "
        f"未变 {same} · 其中语言中立（原文即译文，照样入表）{neutral} · 清掉自映射 {len(pruned)}"
    )
    for k in added:
        print(f"  + {k}  →  {incoming[k]}")
    for k in updated:
        print(f"  ~ {k}\n      旧 {existing[k]}\n      新 {incoming[k]}")

    if not write:
        print("（预演。加 --write 落盘。）")
        return 0

    i18n_table.write(merged, lang)
    print(f"i18n_fill: 已写入 {i18n_table.target(lang)}（{len(merged)} 条）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
