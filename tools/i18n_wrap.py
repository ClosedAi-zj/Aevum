#!/usr/bin/env python3
"""把 UI 构造调用里的中文字面量包成 `crate::i18n::t!(...)`。

只处理**位置明确是给人看的标签**的模式，不做通用字符串扫描 —— 通用扫描会把
CLI 参数解析用的中文别名、日志、`assert!` 里的说明一起改掉，那些一改就出错，
而且编译器不一定拦得住（`match` 模式位置上放函数调用直接语法错，反而是好事；
真正危险的是比较与解析，那种改完还能编译，只是行为悄悄变了）。

用法：
    python tools/i18n_wrap.py --dry-run     # 只列将要改的行
    python tools/i18n_wrap.py --apply       # 真改
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "crates" / "aevum-ui" / "src"

HAN = re.compile(r"[\u4e00-\u9fff]")

# (名字, 正则)：第 1 组是要包的字面量正文。`[^"]*` 足够——界面文案里没有转义引号。
PATTERNS: list[tuple[str, re.Pattern[str]]] = [
    ("RichText::new", re.compile(r'RichText::new\("([^"\\]*)"\)')),
    ("ui.button", re.compile(r'\bui\.button\("([^"\\]*)"\)')),
    ("Button::new", re.compile(r'\bButton::new\("([^"\\]*)"\)')),
    ("ui.menu_button", re.compile(r'\bui\.menu_button\("([^"\\]*)"')),
    ("on_hover_text", re.compile(r'\.on_hover_text\("([^"\\]*)"\)')),
    ("Window::new", re.compile(r'\bWindow::new\("([^"\\]*)"\)')),
    ("hint_text", re.compile(r'\.hint_text\("([^"\\]*)"\)')),
    ("ui.checkbox", re.compile(r'\bui\.checkbox\(&mut [A-Za-z_][A-Za-z0-9_.]*,\s*"([^"\\]*)"\)')),
    ("ui.radio_value", re.compile(r'\bui\.radio_value\(&mut [A-Za-z_][A-Za-z0-9_.]*,\s*\w+,\s*"([^"\\]*)"\)')),
]


def wrap_one(line: str) -> tuple[str, list[str]]:
    """返回 (改后行, 命中的模式名)。已经包过 `t!` 的不重复包。"""
    hits: list[str] = []
    out = line
    for name, pat in PATTERNS:
        def repl(m: re.Match[str]) -> str:
            body = m.group(1)
            if not HAN.search(body):
                return m.group(0)
            if "t!(" in m.group(0):
                return m.group(0)
            hits.append(name)
            start, end = m.span()
            whole = m.group(0)
            # 把第一个 "…" 换成 t!("…")，保留调用形状（括号位置各不相同）
            new = whole.replace(f'"{body}"', f'crate::i18n::t!("{body}")', 1)
            return new

        out = pat.sub(repl, out)
    return out, hits


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--apply", action="store_true")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()
    if not args.apply and not args.dry_run:
        ap.error("指定 --dry-run 或 --apply")

    files = sorted(p for p in SRC.rglob("*.rs") if p.name not in {"i18n.rs", "i18n_en.rs"})
    total = 0
    per_file: dict[str, int] = {}
    for path in files:
        text = path.read_text(encoding="utf-8")
        lines = text.split("\n")
        changed = 0
        for i, line in enumerate(lines):
            if not HAN.search(line):
                continue
            new, hits = wrap_one(line)
            if hits:
                changed += len(hits)
                if args.dry_run:
                    rel = path.relative_to(ROOT)
                    print(f"{rel}:{i + 1}: [{','.join(hits)}] {new.strip()[:110]}")
                lines[i] = new
        if changed and args.apply:
            path.write_text("\n".join(lines), encoding="utf-8")
        if changed:
            per_file[str(path.relative_to(ROOT))] = changed
            total += changed

    print(f"\n合计 {total} 处，涉及 {len(per_file)} 个文件", file=sys.stderr)
    if args.dry_run:
        print("（dry-run，未写盘）", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
