#!/usr/bin/env python3
"""shell.rs 拆分审计工具（性能重构设计-2026-10-30 §9）。

从 Rust 源文件按顶层项（fn / struct / enum / const / static / mod 块）边界切出
每个项的全文，规范化（去可见性前缀、去空行、去行尾空白）后存快照；
拆分后再对目标文件/目录重复切分，按名字对齐比对规范化文本，
要求 100% 逐字一致——任何差异都是"顺手改了代码"的违规证据。

用法：
    python tools/move_audit.py snapshot <src.rs> <out.json>     # 生成快照
    python tools/move_audit.py audit <snapshot.json> <src.rs|dir> [--report out.md]

切分规则：
- 顶层项起点：声明行（可带 pub/pub(crate)/pub(super) 前缀），行首无缩进；
  前面紧贴的 `///` 文档与 `#[...]` 属性随项一起带走（连续行，中间无空行断开）。
- 项体：花括号平衡（带 Rust 字符串/字符/注释扫描器，format! 花括号不误判）；
  const/static 无体的单行声明按分号收尾。
- `use` 语句与零散分隔注释不进快照（拆分允许重组 use 树），但拆分脚本
  自身保证原文不丢。
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

DECL_RE = re.compile(
    r"^(?:(?:pub(?:\((?:crate|super|in [a-z_:]+)\))?\s+)?"
    r"(?:fn|struct|enum|const|static|mod|impl|trait)\b)"
)

# 顶层项声明：必须在行首（无缩进）；带可见性前缀也算。impl/trait 整块算一项。
TOP_DECL_RE = re.compile(
    r"^(?:(?:pub(?:\((?:crate|super|in [a-z_:]+)\))?\s+)?"
    r"(?:fn|struct|enum|const|static|mod|impl|trait)\b)"
)


def scan_braces(text: str, start: int) -> int:
    """从 text[start]（应为 '{'）扫描到配对 '}'，返回其下标。

    正确处理：行注释 //、块注释 /* */（可嵌套）、字符串 "..."（带转义）、
    字符字面量 '..'、原始字符串 r"..." / r#"..."#。
    """
    i = start
    n = len(text)
    depth = 0
    while i < n:
        c = text[i]
        nxt = text[i + 1] if i + 1 < n else ""
        # 原始字符串 r#"..."# / r"..."
        if c == "r" and nxt in ('"', "#") and (
            i == 0 or text[i - 1] not in ("_", "a", "b", "c", "d", "e", "f", "g", "h",
                                          "i", "j", "k", "l", "m", "n", "o", "p", "q",
                                          "r", "s", "t", "u", "v", "w", "x", "y", "z",
                                          "A", "Z", "0", "9", '"', "\\")
        ):
            hashes = 0
            j = i + 1
            while j < n and text[j] == "#":
                hashes += 1
                j += 1
            if j < n and text[j] == '"':
                closer = '"' + "#" * hashes
                end = text.find(closer, j + 1)
                if end == -1:
                    raise ValueError("unterminated raw string")
                i = end + len(closer)
                continue
        # 行注释
        if c == "/" and nxt == "/":
            i = text.find("\n", i)
            if i == -1:
                break
            continue
        # 块注释（嵌套）
        if c == "/" and nxt == "*":
            nest = 1
            i += 2
            while i < n and nest:
                if text.startswith("/*", i):
                    nest += 1
                    i += 2
                elif text.startswith("*/", i):
                    nest -= 1
                    i += 2
                else:
                    i += 1
            continue
        # 字符串
        if c == '"':
            i += 1
            while i < n:
                if text[i] == "\\":
                    i += 2
                    continue
                if text[i] == '"':
                    i += 1
                    break
                i += 1
            continue
        # 字符字面量（含 '\''
        if c == "'":
            # 生命周期：'a 后跟字母且不是 \' 形式——保守判断：找最近的配对 '
            j = i + 1
            if j < n and text[j] == "\\":
                j += 2
                # 跳到闭引号
                while j < n and text[j] != "'":
                    j += 1
                i = j + 1
                continue
            # 生命周期 'ident：后面是字母/下划线且再往后不是 '
            k = j
            while k < n and (text[k].isalnum() or text[k] == "_"):
                k += 1
            if k > j and (k >= n or text[k] != "'"):
                i = k  # 生命周期，跳过标识符
                continue
            # 真字符字面量：跳到闭引号
            while j < n and text[j] != "'":
                if text[j] == "\\":
                    j += 1
                j += 1
            i = j + 1
            continue
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    raise ValueError("unbalanced braces")


def split_items(text: str) -> list[dict]:
    """把源码切成顶层项列表；返回 [{name, kind, header, body}]。

    header 是声明行 + 紧贴其上的文档/属性；body 是从声明行起（含 header 之后的
    所有行）到项结束的原文。项之间游离的空行/分隔注释不进项。
    """
    lines = text.split("\n")
    items: list[dict] = []
    i = 0
    n = len(lines)

    def join_body(header_start: int, end_line: int) -> str:
        return "\n".join(lines[header_start:end_line + 1])

    while i < n:
        line = lines[i]
        # 顶层声明必须顶格（列 0）：缩进的 fn/const（impl 块、测试、函数体内）不算。
        if not line or line[0] in " \t" or not TOP_DECL_RE.match(line):
            i += 1
            continue
        stripped = line.strip()
        # 回收紧贴的文档注释与属性（连续行，可跨空行？不——空行断开归属）。
        header_start = i
        j = i - 1
        while j >= 0:
            prev = lines[j].strip()
            if prev.startswith("///") or prev.startswith("#[") or prev.startswith("#!["):
                header_start = j
                j -= 1
            else:
                break
        kind_m = re.match(
            r"(?:(?:pub(?:\([^)]*\))?\s+)?)(fn|struct|enum|const|static|mod|impl|trait)\b", stripped
        )
        kind = kind_m.group(1)
        if kind == "impl":
            # `impl Trait for Type` 认 Type；`impl Type` 认 Type。
            for_m = re.search(r"\bfor\s+([A-Za-z_][A-Za-z0-9_]*)", stripped)
            if for_m:
                name = "impl@" + for_m.group(1)
            else:
                ids = re.findall(r"([A-Za-z_][A-Za-z0-9_]*)", stripped)
                name = "impl@" + (ids[-1] if ids else "?")
        else:
            name_m = re.search(r"\b(?:fn|struct|enum|const|static|mod|trait)\s+([A-Za-z_][A-Za-z0-9_]*)", stripped)
            name = name_m.group(1)
        # 项体边界：找声明行之后第一个顶层 `{` 或顶层 `;`。
        # 括号感知：`(..)`/`[..]` 里的 `;`（如 `[Color32; 2]`）不算终止符。
        decl_char = line.lstrip()
        offset = len(line) - len(decl_char)
        line_start = sum(len(x) + 1 for x in lines[:i])
        pos = line_start + offset
        seg_end = None
        k = pos
        depth = 0
        while k < len(text):
            ch = text[k]
            if ch == "{" and depth == 0:
                close = scan_braces(text, k)
                seg_end = close
                break
            if ch == ";" and depth == 0:
                seg_end = k
                break
            if ch in "([":
                depth += 1
            elif ch in ")]":
                depth -= 1
            elif ch == '"':
                # 字符串跳过
                k += 1
                while k < len(text) and text[k] != '"':
                    if text[k] == "\\":
                        k += 1
                    k += 1
            elif ch == "/" and k + 1 < len(text) and text[k + 1] == "/":
                nl = text.find("\n", k)
                k = nl if nl != -1 else len(text)
            elif ch == "/" and k + 1 < len(text) and text[k + 1] == "*":
                end = text.find("*/", k + 2)
                k = end + 2 if end != -1 else len(text)
            k += 1
        if seg_end is None:
            raise ValueError(f"item {name}: no terminator")
        end_line = text.count("\n", 0, seg_end)
        # 仅声明的 `mod x;`（无体）是模块结构声明，不作为项参与拆分/审计。
        if kind == "mod" and text[seg_end] == ";":
            i = end_line + 1
            continue
        # 换行符修正：若项结束于行中间（; 情形），收整行。
        items.append({
            "name": name,
            "kind": kind,
            "header_start": header_start,
            "end_line": end_line,
        })
        i = end_line + 1
    # 生成 body 文本；同名项（如同一类型的两个 impl）加 "#2"/"#3" 后缀保证唯一，
    # 后缀按出现顺序确定——快照与拆分后按名对齐不依赖文件顺序。
    out = []
    seen: dict[str, int] = {}
    for it in items:
        name = it["name"]
        if name in seen:
            seen[name] += 1
            name = f"{name}#{seen[name]}"
        else:
            seen[it["name"]] = 1
        body = join_body(it["header_start"], it["end_line"])
        out.append({
            "name": name,
            "kind": it["kind"],
            "header_start": it["header_start"],
            "end_line": it["end_line"],
            "text": body,
        })
    return out


def normalize(item_text: str) -> str:
    """规范化：去可见性前缀、去 use 行（use 树重组允许，见模块 doc）、去空行、去行尾空白。"""
    lines = []
    in_use = False
    for line in item_text.split("\n"):
        line = line.rstrip()
        m = re.match(r"^pub(?:\((?:crate|super|in [a-z_:]+)\))?\s+", line)
        if m:
            # 无论哪种可见性都剥掉（含 pub(crate)/pub(super)）
            line = line[m.end():]
        s = line.strip()
        if in_use:
            if ";" in s:
                in_use = False
            continue
        if s.startswith("use "):
            if ";" not in s:
                in_use = True
            continue
        if s:
            lines.append(line)
    return "\n".join(lines)


def items_map(text: str) -> dict:
    result = {}
    for it in split_items(text):
        norm = normalize(it["text"])
        # 仅声明的 `mod x;`（无体）是模块结构声明，不是被搬运的代码，两侧都不进比对。
        if it["kind"] == "mod" and "{" not in norm:
            continue
        result[it["name"]] = {
            "kind": it["kind"],
            "norm": norm,
        }
    return result


def collect_source(target: Path) -> str:
    if target.is_dir():
        return "\n\n".join(
            p.read_text(encoding="utf-8") for p in sorted(target.rglob("*.rs"))
        )
    return target.read_text(encoding="utf-8")


def main() -> int:
    if len(sys.argv) < 4:
        print(__doc__)
        return 2
    cmd = sys.argv[1]
    if cmd == "snapshot":
        src = Path(sys.argv[2])
        out = Path(sys.argv[3])
        m = items_map(collect_source(src))
        out.write_text(
            json.dumps(m, ensure_ascii=False, indent=1), encoding="utf-8"
        )
        print(f"snapshot: {len(m)} items -> {out}")
        return 0
    if cmd == "audit":
        snap_path = Path(sys.argv[2])
        target = Path(sys.argv[3])
        report_out = None
        if "--report" in sys.argv:
            report_out = Path(sys.argv[sys.argv.index("--report") + 1])
        snap = json.loads(snap_path.read_text(encoding="utf-8"))
        cur = items_map(collect_source(target))
        missing = sorted(set(snap) - set(cur))
        extra = sorted(set(cur) - set(snap))
        diffs = []
        for name in sorted(set(snap) & set(cur)):
            if snap[name]["norm"] != cur[name]["norm"]:
                a = snap[name]["norm"].split("\n")
                b = cur[name]["norm"].split("\n")
                first = next(
                    (k for k in range(max(len(a), len(b)))
                     if k >= len(a) or k >= len(b) or a[k] != b[k]),
                    -1,
                )
                diffs.append({
                    "name": name,
                    "kind": snap[name]["kind"],
                    "lines_snap": len(a),
                    "lines_cur": len(b),
                    "first_diff_line": first + 1,
                    "snap_line": a[first] if first < len(a) else "<EOF>",
                    "cur_line": b[first] if first < len(b) else "<EOF>",
                })
        ok = not missing and not extra and not diffs
        lines = [
            "# move_audit 报告",
            "",
            f"- 快照：`{snap_path}`",
            f"- 目标：`{target}`",
            f"- 快照项数：{len(snap)}　目标项数：{len(cur)}",
            f"- 缺失项：{len(missing)}　多出项：{len(extra)}　文本差异项：{len(diffs)}",
            "",
        ]
        if missing:
            lines.append("## 缺失项")
            lines += [f"- {x}" for x in missing]
            lines.append("")
        if extra:
            lines.append("## 多出项")
            lines += [f"- {x}" for x in extra]
            lines.append("")
        if diffs:
            lines.append("## 文本差异")
            for d in diffs:
                lines += [
                    f"- {d['name']} ({d['kind']}): 快照 {d['lines_snap']} 行 / 目标 {d['lines_cur']} 行，"
                    f"首个差异在第 {d['first_diff_line']} 行",
                    f"  - 快照: `{d['snap_line']}`",
                    f"  - 目标: `{d['cur_line']}`",
                ]
            lines.append("")
        verdict = "PASS：全部顶层项规范化后逐字一致" if ok else "FAIL：存在差异，禁止视为纯移动"
        lines.append(f"## 结论")
        lines.append(f"**{verdict}**")
        report = "\n".join(lines) + "\n"
        print(report)
        if report_out:
            report_out.write_text(report, encoding="utf-8")
        return 0 if ok else 1
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main())
