#!/usr/bin/env python3
"""shell.rs 三步拆分的机械移动脚本（性能重构设计-2026-10-30 §10）。

用法：
    python tools/split_shell.py <mapping.json>

mapping.json 形如：
    {
      "target": "crates/aevum-ui/src/shell",       # 输出目录（shell/）
      "source": "crates/aevum-ui/src/shell.rs",    # 本步拆分前源（第一步是 shell.rs，
                                                    # 之后各步是 shell/mod.rs）
      "modules": {
        "mask":   {"doc": "…模块头注释…", "items": ["MaskGeo", "mask_geos", …]},
        "adjust": {"doc": "…", "items": ["adjust_tab", …]}
      }
    }

行为：
- 项体逐字搬运（与 move_audit 同一切分边界：声明行 + 紧贴的文档/属性 + 项体）；
  项之前的游离注释/空行（段头注释）跟随其后第一个项走。
- 目标模块文件 = 模块头 + 源文件前导 use 树逐字复制 + 项（按原文件相对顺序）。
- 未列出的项与剩余文本原样留在 source；第一步时 shell.rs 重写为 shell/mod.rs。
- 只做移动；可见性修正与 use 剪枝由编译器驱动、人工确认，不属于本脚本。
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from move_audit import split_items  # noqa: E402


def extract_use_block(text: str) -> tuple[str, str]:
    """把文件前导区（第一个顶层项之前）切成 (use 树文本, 其余前导文本)。"""
    lines = text.split("\n")
    use_lines: list[tuple[int, str]] = []
    in_use = False
    first_item = len(lines)
    for i, line in enumerate(lines):
        s = line.strip()
        if s.startswith("use ") or (in_use and not s.startswith("}")) or s.startswith("}"):
            if s.startswith("use ") or in_use:
                use_lines.append((i, line))
                in_use = not s.endswith(";")
                continue
        if not in_use and s and not s.startswith("//") and not s.startswith("//!"):
            first_item = i
            break
    use_text = "\n".join(l for _, l in use_lines)
    rest = "\n".join(l for i, l in enumerate(lines) if i not in {j for j, _ in use_lines})
    return use_text, rest


def main() -> int:
    cfg = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    src = Path(cfg["source"])
    out_dir = Path(cfg["target"])
    text = src.read_text(encoding="utf-8")

    items = split_items(text)
    # 行号辅助：定位每项的 lead 文本（上一项结束到本项 header 之间）。
    lines = text.split("\n")
    by_name = {it["name"]: it for it in items}
    for k, it in enumerate(items):
        lead_start = items[k - 1]["end_line"] + 1 if k else 0
        lead_end = it["header_start"]  # 不含 header
        it["lead"] = "\n".join(lines[lead_start:lead_end])

    use_text, _ = extract_use_block(text.split("#[cfg(test)]")[0] if "#[cfg(test)]" in text and False else text)

    moved_names = set()
    for mod_name, spec in cfg["modules"].items():
        wanted = spec["items"]
        missing = [n for n in wanted if n not in by_name]
        if missing:
            print(f"ERROR: items not found for {mod_name}: {missing}")
            return 1
        moved_names.update(wanted)
        parts = [f"//! {spec['doc']}\n"]
        parts.append("\n// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。\n")
        parts.append(use_text)
        for it in items:
            if it["name"] in wanted:
                parts.append(it["lead"])
                parts.append(it["text"])
        out = out_dir / f"{mod_name}.rs"
        out.parent.mkdir(parents=True, exist_ok=True)
        body = "\n".join(parts)
        # 压平 3+ 连续空行
        while "\n\n\n\n" in body:
            body = body.replace("\n\n\n\n", "\n\n\n")
        out.write_text(body, encoding="utf-8", newline="\n")
        print(f"wrote {out} ({len(wanted)} items)")

    # 重写 source：去掉已移走的项（连同其 lead）。
    drop_spans = []
    for it in items:
        if it["name"] in moved_names:
            lead_start = 0
            for prev in items:
                if prev is it:
                    break
                lead_start = prev["end_line"] + 1
            # 找本项在 items 里的序号
            idx = items.index(it)
            lead_start = items[idx - 1]["end_line"] + 1 if idx else 0
            drop_spans.append((lead_start, it["end_line"]))
    keep = [True] * len(lines)
    for a, b in drop_spans:
        for i in range(a, b + 1):
            keep[i] = False
    remainder = "\n".join(l for i, l in enumerate(lines) if keep[i])
    while "\n\n\n\n" in remainder:
        remainder = remainder.replace("\n\n\n\n", "\n\n\n")
    src.write_text(remainder, encoding="utf-8", newline="\n")
    print(f"rewrote {src} (kept {len(items) - len([i for i in items if i['name'] in moved_names])} items)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
