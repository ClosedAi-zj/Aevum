#!/usr/bin/env python3
"""扫描源码里的可翻译文案，与英文表比对，报告缺译。

能看见的来源（**只是发现手段之一，不是完整性判据**）：

1. 代码里显式的 `crate::i18n::t!("…")` 与 `tr_fmt("…")` 模板；
2. 界面辅助函数的字符串参数（`bar_button(ui, "取消")` 这类）——翻译下沉在函数
   内部，代码里没有 `t!` 标记，只能按**参数位置**认。

看不见的来源同样合法：`controls::SPECS`、`grade::PRESET_NAMES`、各 `cn()` 枚举
（数据表，键在取值处才过 `tr`）、`control.rs` 的命令说明（在 AI 面板渲染处翻译）。
所以**表里多出来的键不一定是脏数据**，本脚本不据此报错、`--write` 也绝不删它们。

真正的守门人是 `crates/aevum-ui/tests/i18n_coverage.rs`：把界面用英文画一遍，
扫渲染结果里的每一个字——那里红才是真缺。

第 2 类必须按白名单逐个函数声明参数位置：通用扫描「所有中文字符串」会把 CLI 的中文
别名（`"序列" | "sequence" => ...`）、日志、`assert!` 说明一起抽进来，译了反而出错。

用法：
    python tools/i18n_extract.py            # 报告缺译
    python tools/i18n_extract.py --write    # 把扫描到的新键并进英文表（只增不删）
    python tools/i18n_extract.py --skeleton # 另存一份待填写的条目骨架
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "crates" / "aevum-ui" / "src"
EN_FILE = SRC / "i18n_en.rs"

HAN = re.compile(r"[\u4e00-\u9fff]")

# 显式标记：宏调用的字面量。`t!(` 与字符串之间允许换行（rustfmt 会拆行）。
T_MACRO = re.compile(r'\bt!\(\s*"([^"\\]*)"', re.S)
# tr_fmt 的首参是模板，同样要进表。
TR_FMT = re.compile(r'\btr_fmt\(\s*"((?:[^"\\]|\\.)*)"', re.S)

# 界面辅助函数 → 显示文案所在的参数位置（从 0 数，字符串参数计）。
# 一条文案可能落在多个位置的，给列表（如 ctx_seg 的 name 与 note、slider_row 的
# label 与 tip）。只登记「参数就是给人看的标签」的函数；参数先是 ID 再是标签的，
# 写它真正的位置。
AUX: dict[str, list[int]] = {
    "bar_button": [1],
    "bar_primary": [1],
    "bar_button_on": [1],
    "bar_chip": [1],
    "note": [1],
    "kv": [1],
    "menu_item": [1],
    "look_section": [1],
    "look_row": [1],
    "group_header": [3],
    "side_head": [1],
    "side_row": [1],
    "side_note": [1],
    "ctx_seg": [1, 2],
    "slider_row": [1, 12],
}

# 这些行一律跳过：日志、断言、测试夹具、以及 CLI 解析用的中文别名。
SKIP_LINE = re.compile(
    r"eprintln!|println!|dbg!|assert!|assert_eq!|assert_ne!|debug_assert"
    r"|=>\s*Some\(|^\s*\"[^\"]*\"\s*\|"  # match 的模式位置 / 解析分支
)


def aux_pattern(name: str, idx: int) -> re.Pattern[str]:
    """构造 `name(a, b, …)` 里第 idx 个参数的匹配。

    参数用「前面恰好有 idx 个未嵌套的逗号」表达。界面代码这几个函数的实参里
    不会出现带逗号的嵌套调用（画家闭包都在外层），所以不做括号配平。
    """
    return re.compile(r"\b" + name + r"\(" + r"(?:[^,()]*,){" + str(idx) + r"}\s*\"([^\"\\]*)\"")


AUX_PATTERNS = [(n, [aux_pattern(n, i) for i in idxs]) for n, idxs in AUX.items()]


def collect() -> dict[str, set[str]]:
    """返回 {中文原文: {出现位置}}。"""
    found: dict[str, set[str]] = {}
    for path in sorted(SRC.rglob("*.rs")):
        if path.name in {"i18n.rs", "i18n_en.rs"}:
            continue
        rel = str(path.relative_to(ROOT))
        for lineno, line in enumerate(path.read_text(encoding="utf-8").split("\n"), 1):
            if not HAN.search(line) or SKIP_LINE.search(line):
                continue
            for m in T_MACRO.finditer(line):
                body = m.group(1)
                if HAN.search(body):
                    found.setdefault(body, set()).add(f"{rel}:{lineno}")
            for m in TR_FMT.finditer(line):
                body = m.group(1)
                if HAN.search(body):
                    found.setdefault(body, set()).add(f"{rel}:{lineno} (tr_fmt)")
            # 宏已经包住的就不再按辅助函数重复记一遍（同一行两种情况都可能出现）。
            for name, pats in AUX_PATTERNS:
                for pat in pats:
                    for m in pat.finditer(line):
                        body = m.group(1)
                        if HAN.search(body):
                            found.setdefault(body, set()).add(f"{rel}:{lineno} ({name})")
    return found


def parse_en_table() -> dict[str, str]:
    """整表读出来。解析规则只在 `i18n_table.py` 有一份，这里不许再抄。"""
    import i18n_table

    return i18n_table.load()[1]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--skeleton", action="store_true", help="把缺译条目写成待填骨架")
    ap.add_argument("--write", action="store_true", help="把扫描到的新键并进英文表（只增不删，按码点排序）")
    args = ap.parse_args()

    found = collect()
    table = parse_en_table()
    missing = sorted(k for k in found if k not in table)
    outside = sorted(k for k in table if k not in found)

    print(f"字面量扫描到的文案：{len(found)} 条", file=sys.stderr)
    print(f"英文表：{len(table)} 条；缺译 {len(missing)} 条", file=sys.stderr)
    print(
        f"表里另有 {len(outside)} 条不来自字面量扫描（数据表 / 命令说明 / tr_fmt 模板），正常。",
        file=sys.stderr,
    )

    if args.write:
        import i18n_table

        merged = {**table, **{k: table.get(k, "") for k in missing}}
        i18n_table.write(merged)
        print(f"已重写 {i18n_table.TARGET.relative_to(ROOT)}：{len(merged)} 条", file=sys.stderr)

    if missing:
        print("\n缺译（按出现位置）：", file=sys.stderr)
        for k in missing:
            where = sorted(found[k])[0]
            print(f"  {k!r}  ← {where}", file=sys.stderr)

    if args.skeleton:
        out = ROOT / "i18n-missing.txt"
        out.write_text(
            "\n".join(f'    ("{k}", ""),' for k in missing) + "\n", encoding="utf-8"
        )
        print(f"\n骨架写到 {out}", file=sys.stderr)

    # 缺译不算失败（回退中文是设计行为）；表里多出来的键也不再算脏数据——
    # 它们来自本脚本看不见的来源，删了才是事故。
    return 0


if __name__ == "__main__":
    sys.exit(main())
