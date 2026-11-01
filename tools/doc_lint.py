#!/usr/bin/env python3
"""代码注释双语守门：生产代码里的模块级 `//!` 与 `pub` 项上的 `///` 必须有英文块。

口径（与 aevum-core / aevum-mcp / aevum-ui 三轮施工一致）：
- 中文块在前，一个空的同标记行（`///` 或 `//!`），英文块在后；
- 只要求**模块级注释**与**挂在 `pub` 项上的注释**双语；
  私有项、`pub(crate)`、测试目录与 examples 不在要求内——
  内部注释是给自己看的，翻成英文只会让改代码时多改一处。

为什么只查「有没有英文块」，不查中英之间那行空行：
英文块里会引用中文文件名（如 `<final-render stem>_帧`），机器无法稳定区分
「中文段的续行」与「英文段里夹的中文字面量」，硬查会常态化误报。
空行那一条靠 review，`docx.py` 注入时也会自动补上。

用法：
    python tools/doc_lint.py            # 检查，红则退出码 1
    python tools/doc_lint.py --list     # 只列出待补的位置，不判失败
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE_DIRS = ["crates", "apps"]
# 测试与示例不要求：那是回归靶子和用法演示，不是交付的 API 面。
SKIP_PARTS = {"tests", "examples", "target"}


def has_cjk(s: str) -> bool:
    return any("\u4e00" <= ch <= "\u9fff" for ch in s)


def has_english(s: str) -> bool:
    return bool(re.search(r"[A-Za-z]{3,}", s)) and not has_cjk(s)


def blocks_of(path: Path):
    """产出 (起始行号, marker, 缩进, 正文行) —— 只含模块级与 pub 项上的块。"""
    lines = path.read_text(encoding="utf-8").split("\n")
    i = 0
    while i < len(lines):
        m = re.match(r"^(\s*)(///|//!)", lines[i])
        if not m:
            i += 1
            continue
        indent, marker = m.group(1), m.group(2)
        j = i
        while j < len(lines) and re.match("^" + indent + re.escape(marker), lines[j]):
            j += 1
        k = j
        while k < len(lines) and not lines[k].strip():
            k += 1
        target = lines[k] if k < len(lines) else ""
        if marker == "//!" or re.match(r"^\s*pub\s", target):
            body = [re.sub("^" + indent + re.escape(marker) + r" ?", "", ln) for ln in lines[i:j]]
            yield i + 1, marker, indent, body, target.strip()
        i = j


def main() -> int:
    list_only = "--list" in sys.argv
    missing = []
    total = 0
    for d in SOURCE_DIRS:
        for path in sorted((ROOT / d).rglob("*.rs")):
            if SKIP_PARTS & set(path.parts):
                continue
            for line, marker, _indent, body, target in blocks_of(path):
                total += 1
                if not any(has_english(b) for b in body):
                    missing.append((path.relative_to(ROOT), line, target[:70]))

    print(f"检查了 {total} 个块（模块级与 pub 项），缺英文 {len(missing)} 个")
    for path, line, target in missing:
        print(f"  {path}:{line}  ->  {target}")
    if not missing:
        return 0
    if list_only:
        return 0
    print(
        "\n修法：中文块之后补一个空的同标记行（`///` 或 `//!`），再写英文块。\n"
        "口径见本脚本头部；批量注入可用 E:\\tmp\\docx.py 那种两段式（scan → apply）。"
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
