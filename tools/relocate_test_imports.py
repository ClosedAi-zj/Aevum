#!/usr/bin/env python3
"""把「仅 cfg(test) 代码在用」的 import 项从模块顶层挪进宿主测试模块（v2，幂等）。

lib 编译报 unused、test 编译不报的项 = 只被测试代码用。
- 单项语句：整条挪（super:: → crate::shell:: 改写）；
- 组语句里的项：整条语句重写为去掉该项的单行组（use 树格式允许重组），
  宿主测试模块加 `use <head>::<name>;`（已存在则跳过 → 幂等）。
mod.rs 的 reset_group 重导出按设计保留，跳过。

用法：python tools/relocate_test_imports.py
"""

from __future__ import annotations

import re
import subprocess
from pathlib import Path

ROOT = Path("E:/code/lumen-studio")
SHELLD = ROOT / "crates/lumen-ui/src/shell"


def collect_lib_warnings() -> dict[str, dict[tuple[int, int], str]]:
    subprocess.run(["touch"] + [str(p) for p in SHELLD.glob("*.rs")], capture_output=True)
    proc = subprocess.run(["cargo", "check", "-p", "lumen-ui", "--message-format=short"],
                          capture_output=True, text=True, cwd=str(ROOT))
    warns: dict[str, dict[tuple[int, int], str]] = {}
    pending = False
    for l in (proc.stdout + proc.stderr).split("\n"):
        m = re.match(r"^(crates[\\/].*?shell[\\/]\w+\.rs):(\d+):(\d+): warning: unused import", l)
        if m:
            names = re.findall(r"`([^`]+)`", l)
            name = names[-1].split("::")[-1] if names else "?"
            warns.setdefault(str(ROOT / m.group(1).replace("\\", "/")), {})[(int(m.group(2)), int(m.group(3)))] = name
            pending = False
            continue
        if "warning: unused import" in l:
            pending = True
            continue
        if pending:
            m = re.match(r"\s*--> (crates[\\/].*?shell[\\/]\w+\.rs):(\d+):(\d+)", l)
            if m:
                warns.setdefault(str(ROOT / m.group(1).replace("\\", "/")), {})[(int(m.group(2)), int(m.group(3)))] = "?"
            pending = False
    return warns


def use_stmt_span(ls: list[str], ln0: int) -> tuple[int, int] | None:
    s = ln0
    while s >= 0 and not ls[s].startswith("use "):
        s -= 1
    if s < 0:
        return None
    e = s
    while e < len(ls) and not ls[e].rstrip().endswith(";"):
        e += 1
    return (s, e) if e < len(ls) else None


def parse_group(stmt: str) -> tuple[str, list[str]] | None:
    m = re.match(r"^use ([\w:]+)::\{(.*)\};\s*$", stmt, re.S)
    if not m:
        return None
    head, inner = m.group(1), m.group(2)
    items = [x.strip() for x in inner.split(",") if x.strip()]
    return head, items


def main() -> int:
    for fname, wmap in sorted(collect_lib_warnings().items()):
        if fname.endswith("mod.rs"):
            print("SKIP mod.rs（reset_group 重导出按设计保留）")
            continue
        p = Path(fname)
        ls = p.read_text(encoding="utf-8").split("\n")
        mods = []
        for i, l in enumerate(ls):
            m = re.match(r"^mod (\w+) \{$", l)
            if m:
                depth = 0
                for j in range(i, len(ls)):
                    depth += ls[j].count("{") - ls[j].count("}")
                    if depth == 0:
                        mods.append((m.group(1), i, j))
                        break

        stmts: dict[tuple[int, int], list[str]] = {}
        for (ln, col), name in sorted(wmap.items()):
            sp = use_stmt_span(ls, ln - 1)
            if sp is None:
                continue
            stmts.setdefault(sp, []).append(name)

        drop: set[int] = set()
        replace: dict[int, str] = {}
        inserts: dict[int, list[str]] = {}
        for (s, e), names in sorted(stmts.items()):
            stmt = "\n".join(ls[s:e + 1])
            all_names = set(re.findall(r"[A-Za-z_]\w*", stmt)) - {
                "use", "crate", "super", "self", "std", "egui", "lumen_core", "as"}
            hosts = []
            for mname, a, b in mods:
                used = any(
                    not l.strip().startswith(("use ", "//"))
                    and any(re.search(rf"\b{n}\b", l) for n in all_names)
                    for l in ls[a + 1:b])
                if used:
                    hosts.append((mname, a, b))
            if not hosts:
                print("KEEP（无宿主测试模块，留在原处）:", Path(fname).name, "←", stmt[:60].replace("\n", " "))
                continue
            grp = parse_group(stmt)
            if grp is None:
                # 单项语句：整条挪，路径改写
                moved = [rewrite if (rewrite := re.sub(r"\buse super::", "use crate::shell::", x)) else x
                         for x in stmt.split("\n")]
                for mname, a, b in hosts:
                    k = a + 1
                    while k < b and ls[k].strip().startswith("use "):
                        k += 1
                    inserts.setdefault(k, []).extend("    " + x if x else x for x in moved)
                drop.update(range(s, e + 1))
                print(f"MOVE → {','.join(h[0] for h in hosts)} : {Path(fname).name} ← {stmt[:60]}")
            else:
                head, items = grp
                kept = [x for x in items if x.split("::")[-1] not in names]
                if kept:
                    replace[s] = f"use {head}::{{{', '.join(kept)}}};"
                else:
                    drop.update(range(s, e + 1))
                for name in names:
                    for mname, a, b in hosts:
                        k = a + 1
                        while k < b and ls[k].strip().startswith("use "):
                            k += 1
                        line = f"use {head}::{name};"
                        if line not in ls[a:b + 1] and line not in inserts.get(k, []):
                            inserts.setdefault(k, []).append(line)
                    print(f"GROUP-MOVE {name} ({head}) → {','.join(h[0] for h in hosts)} : {Path(fname).name}")

        if not drop and not replace and not inserts:
            continue
        out = []
        for i, l in enumerate(ls):
            if i in inserts:
                out.extend("    " + x if not x.startswith("    ") and x else x for x in inserts[i])
            if i in drop:
                continue
            if i in replace:
                out.append(replace[i])
                continue
            out.append(l)
        text = "\n".join(out)
        text = re.sub(r"^use ([\w:]+)::\{\s*\};\s*\n?", "", text, flags=re.M)
        Path(fname).write_text(text, encoding="utf-8", newline="\n")
    print("done")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
