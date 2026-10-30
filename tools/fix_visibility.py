#!/usr/bin/env python3
"""拆分后的可见性/use 修复循环：按 cargo check 的 E0425/E0422 逐个修。

- 「cannot find X in this scope」（子模块 F 引用兄弟模块的 X）：
  在 X 的定义处补 pub(super)（已是 pub(crate)/pub 则不动），在 F 顶部补
  use super::<mod>::X;（mod.rs 用 use self::<mod>::X;）。
- 「cannot find X in module `crate::shell`」（state.rs 等外部路径）：
  在 mod.rs 补 pub(crate) use <mod>::X; 重导出，外部路径保持不变。

可见性规则（§8）：能 pub(super) 不 pub(crate)；仅外部（state.rs）在用的升 pub(crate)。
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

SRC = Path("E:/code/lumen-studio/crates/lumen-ui/src")
SHELL = SRC / "shell"
DECL = re.compile(r"^(?:(?:pub(?:\((?:crate|super)\))?\s+)?)(fn|struct|enum|const|static|mod|type)\s+([A-Za-z_]\w*)")


def build_index() -> dict:
    """名字 -> (文件, 模块名, 当前行号, 当前可见性)。"""
    idx = {}
    for f in sorted(SHELL.glob("*.rs")):
        modname = "mod" if f.name == "mod.rs" else f.stem
        for i, line in enumerate(f.read_text(encoding="utf-8").split("\n")):
            m = DECL.match(line)
            if m:
                name = m.group(2)
                vis = "pub(crate)" if line.startswith("pub(crate)") else (
                    "pub(super)" if line.startswith("pub(super)") else (
                        "pub" if line.startswith("pub") else "priv"))
                # 同名项取第一个（测试模块里的 fn 都有缩进，不会进索引）
                idx.setdefault(name, (f, modname, i, vis))
    return idx


def insert_use_line(path: Path, modname: str, target_mod: str, name: str, reexport: bool) -> None:
    text = path.read_text(encoding="utf-8")
    lines = text.split("\n")
    if reexport:
        imp = f"pub(crate) use {target_mod}::{name};" if target_mod != "mod" else f"pub(crate) use {name};"
    elif target_mod == "mod":
        # 目标是 shell/mod.rs（父模块）：子模块里用 `use super::X;`
        imp = f"use super::{name};"
    else:
        imp = f"use self::{target_mod}::{name};" if modname == "mod" else f"use super::{target_mod}::{name};"
    if imp in lines:
        return
    # 找最后一个现有 use 行，插到其后；找不到就插在文档头之后第一个空行前
    last_use = max((i for i, l in enumerate(lines) if l.startswith("use ")), default=None)
    if last_use is None:
        # 插到 //! 头之后
        i = 0
        while i < len(lines) and (lines[i].startswith("//!") or not lines[i].strip()):
            i += 1
        lines.insert(i, imp)
    else:
        lines.insert(last_use + 1, imp)
    path.write_text("\n".join(lines), encoding="utf-8", newline="\n")


def bump_visibility(idx, name: str, want: str) -> None:
    f, modname, line_no, vis = idx[name]
    if vis in ("pub", "pub(crate)") or (want == "pub(super)" and vis == "pub(super)"):
        return
    lines = f.read_text(encoding="utf-8").split("\n")
    # 行号可能在同一轮里因插入 use 而漂移——按名字重新定位声明行，避免改错行。
    pat = re.compile(
        r"^(?:(?:pub(?:\((?:crate|super)\))?\s+)?)"
        r"(?:fn|struct|enum|const|static|mod|type)\s+" + re.escape(name) + r"\b"
    )
    found = None
    for i, line in enumerate(lines):
        if pat.match(line):
            found = i
            break
    if found is None:
        return
    line = lines[found]
    if vis == "priv":
        lines[found] = f"pub(super) {line}" if want == "pub(super)" else f"pub(crate) {line}"
    elif vis == "pub(super)" and want == "pub(crate)":
        lines[found] = line.replace("pub(super)", "pub(crate)", 1)
    else:
        return
    f.write_text("\n".join(lines), encoding="utf-8", newline="\n")
    idx[name] = (f, modname, found, want)


def main() -> int:
    max_rounds = int(sys.argv[1]) if len(sys.argv) > 1 else 12
    for rnd in range(max_rounds):
        idx = build_index()
        proc = subprocess.run(
            ["cargo", "check", "--all-targets", "--message-format=short", "-p", "lumen-ui"],
            capture_output=True, text=True, cwd="E:/code/lumen-studio",
        )
        out = proc.stdout + proc.stderr
        errs = [l for l in out.split("\n") if ": error[E" in l]
        if not errs:
            print(f"round {rnd}: clean")
            # 顺手报 unused import 警告数
            warns = [l for l in out.split("\n") if "unused import" in l]
            print(f"unused imports: {len(warns)}")
            return 0
        fixed = 0
        for line in errs:
            m = re.search(r"^(.+?):(\d+):\d+: error\[E\d+\]: (.+)$", line)
            if not m:
                continue
            err_file, _, msg = m.group(1), m.group(2), m.group(3)
            err_file = (Path("E:/code/lumen-studio") / err_file.replace("\\", "/")).resolve()
            mm = re.search(r"cannot find (?:type|struct, variant or union type|function|value) `([^`]+)`", msg)
            if not mm:
                continue
            name = mm.group(1)
            m2 = re.search(r"in module `crate::shell`", msg)
            if name not in idx:
                continue
            f, defmod, _, _ = idx[name]
            if m2:  # 外部路径 crate::shell::X → 重导出
                insert_use_line(SHELL / "mod.rs", "mod", defmod, name, reexport=True)
                bump_visibility(idx, name, "pub(crate)" if f.name == "mod.rs" else "pub(super)")
                fixed += 1
            elif err_file != f:
                # 跨子模块引用：升 pub(super) + use
                if defmod != "mod":
                    bump_visibility(idx, name, "pub(super)")
                target = defmod
                insert_use_line(err_file, err_file.stem if err_file.name != "mod.rs" else "mod", target, name, reexport=False)
                fixed += 1
        print(f"round {rnd}: {len(errs)} errors, applied {fixed} fixes")
        if fixed == 0:
            for line in errs[:15]:
                print("  UNFIXED:", line.strip()[:160])
            return 1
    return 1


if __name__ == "__main__":
    sys.exit(main())
