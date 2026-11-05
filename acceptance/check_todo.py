#!/usr/bin/env python3
"""生产代码「活 TODO」闸门（acceptance/check.sh 第 0 关）。

口径（中文）：只扫 `crates/` 与 `apps/` 下的 `*.rs`，命中正则 `TODO\\(` 或 `FIXME`
即判红，打印 `文件:行号: 内容` 并退出 1。这是**窄口径**：`acceptance/` 金标 note、
注释里引用上游 rawimage.rs 历史片段的裸 `TODO fixme` 叙述、`docs/`、`tools/`、
`scripts/` 都不在扫描范围——它们是叙述或数据，不是活待办。全仓宽扫会命中这些字面，
所以「0 条」这个结论必须绑定在这条判据上，而不是一句手写 grep。

为什么这条闸存在：**决定不做也是关闭，但必须把代码里的 TODO 标记删掉**。一个功能被
明确判定「不做」之后，留在生产源码里的 `TODO(...)` / `FIXME` 仍会让下一个读者以为还有
未收口的工作；反过来，真有活待办时也不该靠人记得 grep。把口径固化成第 0 关脚本，
两边都不靠记忆。

自举：读 `acceptance/check.sh`，剔除行首 `#` 的注释行后，必须仍有真实调用
`acceptance/check_todo.py`；否则判红。防止有人把调用挪进注释或 echo 里而门槛假绿
（与 `acceptance/check_release.py` 的 R9 同款手法）。

Scope (English): this scans only `*.rs` under `crates/` and `apps/`; a line matching
`TODO\\(` or `FIXME` fails the gate with `file:line: content` and exit code 1. It is
deliberately narrow: golden notes under `acceptance/`, bare `TODO fixme` prose in comments
quoting upstream rawimage.rs history, `docs/`, `tools/` and `scripts/` are out of scope --
they are narrative or data, not live to-dos. A repository-wide grep hits those literals, so
the "zero" claim is pinned to this gate rather than to a hand-written command.

Why the gate exists: **deciding not to do something also closes it, but the TODO marker still
has to be deleted from the code**. A `TODO(...)` / `FIXME` left in production source reads as
unfinished work to the next reader; conversely a genuine live to-do should not depend on
someone remembering to grep. Gate 0 makes both sides mechanical.

Bootstrap (English): after dropping `#` comment lines, `acceptance/check.sh` must still
contain a real call to `acceptance/check_todo.py` -- the same technique as R9 in
`acceptance/check_release.py`, so a call hidden in a comment or an echo cannot fake a green.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE_DIRS = ("crates", "apps")
MARKER = re.compile(r"TODO\(|FIXME")
GATE = "acceptance/check.sh"
SELF_CALL = "acceptance/check_todo.py"
# 只认「python 真调了一行」，光把文件名写进 echo 不算数。
# Only a real `python ...` invocation counts; echoing the filename does not.
SELF_CALL_RE = re.compile(r"^\s*python\s+.*acceptance/check_todo\.py", re.MULTILINE)


def find_markers() -> list[str]:
    """返回 `文件:行号: 内容` 列表；按路径、行号排序，便于直接跳过去改。

    Return every `file:line: content` hit, sorted by path and line number so the
    report points straight at the source.
    """
    problems: list[str] = []
    for directory in SOURCE_DIRS:
        for path in sorted((ROOT / directory).rglob("*.rs")):
            text = path.read_text(encoding="utf-8", errors="replace")
            for lineno, line in enumerate(text.splitlines(), start=1):
                if MARKER.search(line):
                    rel = path.relative_to(ROOT).as_posix()
                    problems.append(f"{rel}:{lineno}: {line.strip()}")
    return problems


def bootstrap_problem() -> str | None:
    """自举：剔除注释行后，check.sh 里必须还有对本判据的真实调用。

    Bootstrap: after comment lines are removed, check.sh must still call this gate.
    """
    gate = ROOT / GATE
    if not gate.is_file():
        return f"check_todo: 缺少 {GATE}，无法确认本判据真的挂在门槛上"
    real_lines = "\n".join(
        line
        for line in gate.read_text(encoding="utf-8").splitlines()
        if not line.lstrip().startswith("#")
    )
    if not SELF_CALL_RE.search(real_lines):
        return (
            f"check_todo: 自举判据失败 —— {GATE} 第 0 关没有真实调用 {SELF_CALL}"
            "（注释行与 echo 不算，写了不跑等于没写）"
        )
    return None


def main() -> int:
    problems: list[str] = find_markers()
    boot = bootstrap_problem()
    if boot is not None:
        problems.append(boot)
    if problems:
        print("生产代码还有活 TODO/FIXME 标记（或本判据没挂上）：", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 1
    print(
        "活 TODO 检查通过：crates/ 与 apps/ 的 *.rs 无 TODO( / FIXME，判据已挂第 0 关 / "
        "Gate passed: no live TODO markers in production Rust, gate 0 is wired"
    )
    return 0


def _utf8_streams() -> None:
    """把 stdout/stderr 掰成 UTF-8：英文 Windows 默认 cp1252，打印中文会抛异常。

    Pin stdout/stderr to UTF-8: English Windows defaults to cp1252, so a Chinese
    report line would raise UnicodeEncodeError instead of showing the result.
    """
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="replace")


if __name__ == "__main__":
    _utf8_streams()
    sys.exit(main())
