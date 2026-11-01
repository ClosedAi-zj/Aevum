#!/usr/bin/env python3
"""`i18n_en.rs` 英文表的读写公共件，`i18n_extract.py` 与 `i18n_fill.py` 共用。

拆成公共模块的理由：两个脚本都要「解析整表 → 合并 → 重写」，解析规则一旦分头实现，
一边容忍 rustfmt 折行、一边按行匹配，就会出现「一个脚本能读、另一个脚本一写就丢
193 条」的事故——2026-10-31 真的发生过一次，靠 `tests/i18n_coverage.rs` 才兜住。

表文件的约定（两边共同维护）：

* 文件头到 `pub(crate) static ENTRIES` 为止是文档注释，重写时**原样保留**；
* 每条一行 `("键", "值"),`，按键的码点排序；`#[rustfmt::skip]` 保证 rustfmt
  不把长条目折成多行——折行本身合法，但单行形态是两个脚本共同的稳定口径。
"""

from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "crates" / "aevum-ui" / "src"

#: 默认语言。加一种语言时不必改这里：`--lang ja` 会指向 `i18n_ja.rs`，
#: 只要那张表的骨架与本文件约定一致（`ENTRIES` 标记 + 一条一行）就能读写。
#:
#: Default language. Adding a language does not touch this: `--lang ja` points at
#: `i18n_ja.rs`, and any table following the same convention (the `ENTRIES` marker,
#: one entry per line) can be read and rewritten.
DEFAULT_LANG = "en"


def target(lang: str = DEFAULT_LANG) -> Path:
    """某种语言的表文件。语言代码直接拼进文件名，没有白名单。"""
    return SRC / f"i18n_{lang}.rs"


TARGET = target()

MARKER = "pub(crate) static ENTRIES: &[(&str, &str)] = &["

# 一条 = `(` 空白 "键" 空白 , 空白 "值" 空白 [尾逗号] `)`。用 re.S 兼容折行——
# rustfmt 折出来的形态是 `"值",`（带尾逗号），不认这个逗号就会静默丢掉整批条目。
ENTRY = re.compile(r'\(\s*("(?:[^"\\]|\\.)*")\s*,\s*("(?:[^"\\]|\\.)*")\s*,?\s*\)', re.S)


def unescape(lit: str) -> str:
    """Rust 字符串字面量 → Python 字符串。本表只可能出现 `\\n` `\\t` `\\"` `\\\\`。"""
    body = lit[1:-1]
    out: list[str] = []
    i = 0
    while i < len(body):
        c = body[i]
        if c == "\\" and i + 1 < len(body):
            nxt = body[i + 1]
            out.append({"n": "\n", "t": "\t", "r": "\r", '"': '"', "\\": "\\"}.get(nxt, nxt))
            i += 2
        else:
            out.append(c)
            i += 1
    return "".join(out)


def escape(s: str) -> str:
    """Python 字符串 → Rust 字面量内容。只转义真会影响解析的字符。"""
    return s.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n").replace("\t", "\\t")


def parse(text: str, path: Path = TARGET) -> tuple[str, dict[str, str]]:
    """返回 (文件头含 MARKER 的前缀, 键值表)。解析不到 MARKER 视为文件被手改。"""
    at = text.find(MARKER)
    if at < 0:
        raise SystemExit(f"i18n_table: {path} 里找不到 ENTRIES 标记，文件被改动过？")
    head = text[: at + len(MARKER)]
    table = {unescape(m.group(1)): unescape(m.group(2)) for m in ENTRY.finditer(text[at:])}
    return head, table


def load(lang: str = DEFAULT_LANG) -> tuple[str, dict[str, str]]:
    path = target(lang)
    if not path.exists():
        raise SystemExit(
            f"i18n_table: {path} 不存在。新增语言先建表骨架（复制 i18n_en.rs 的文件头 + "
            f"空 ENTRIES 数组），再用 --lang {lang} 填值。"
        )
    return parse(path.read_text(encoding="utf-8"), path)


def render(head: str, table: dict[str, str]) -> str:
    """整表按键码点排序，一条一行。表体必须停在单行形态，两个脚本都靠它。"""
    body = "".join(f'    ("{escape(k)}", "{escape(table[k])}"),\n' for k in sorted(table))
    return f"{head}\n{body}];\n"


def write(table: dict[str, str], lang: str = DEFAULT_LANG) -> None:
    """整表重写。文件头（含 `#[rustfmt::skip]`）原样保留。"""
    head, _ = load(lang)
    target(lang).write_text(render(head, table), encoding="utf-8", newline="\n")
