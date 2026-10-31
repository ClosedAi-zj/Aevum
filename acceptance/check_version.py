#!/usr/bin/env python3
"""版本一致性检查（check.sh 的第 0 关）。

为什么存在：beta.2 发出去当天才发现 README 还是旧版本号——8 处版本号全靠
人肉同步，漂移过一次就是证据。本脚本把「Cargo.toml 唯一真源 → 其余各处」
的同步变成提交门槛，漂移在 check.sh 就拦下，不再等发版当天。

检查口径（1.0.0 验收清单 B5）：
  1. Cargo.toml 的 version 是唯一真源；
  2. Cargo.lock 四个成员（aevum-core/mcp/ui/studio）与真源一致（cargo 会自动
     回写，漂移说明有人手改了 lock 或忘了 cargo check）；
  3. README.md 四处版本语境恰好各 1 处且与真源一致——中英各两处，且两处落在
     不同位置（页首状态行 + 版本方案节），所以「改了一处忘了另一处」也会红：
     - 中文页首「**当前版本**：`x`」、英文页首「**Current version**: `x`」；
       这两处允许写 `X.Y.Z-beta.x` 占位形式，此时只校验主版本号部分（X.Y.Z）
       与真源一致——beta 序号推进不必改 README；
     - 中文版本方案节「当前为 `x`」、英文版本方案节「the current release is `x`」；
       这两处必须是完整的当前版本号，占位形式不认。
     1.0.0（无 beta 后缀）天然兼容：页首直写 `1.0.0` 时四处都按完整版本号比对。
  4. CHANGELOG.md 第一个 `## vX.Y.Z — 日期` 标题与真源一致。

四处语境的措辞会随对外承诺演进（B6 那轮把「beta 期不承诺」换成了 1.x 的四类兼容
承诺，页首那两处 `beta 阶段（…）` 也就跟着改成了 `**当前版本**：…`）。改措辞时**只
改下面 README_PATTERNS 里的正则**，不改「四处、每处恰好 1 次」这个口径——语境可以
换说法，数量与位置不能少，否则漂移又没人拦。
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

# 仓库根从脚本自身位置推出来，不写死盘符和目录名（理由同 tools/fix_visibility.py）。
ROOT = Path(__file__).resolve().parents[1]

# 完整版本号：1.2.3 或 1.2.3-beta.2（pre-release 只认点分字母数字段）。
VER = r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?"
# beta 占位形式：X.Y.Z-beta.x（README 里「beta 阶段（`x`）」用的就是这种）。
VER_BETA_X = r"\d+\.\d+\.\d+-beta\.x"

LOCK_MEMBERS = ("aevum-core", "aevum-mcp", "aevum-ui", "aevum-studio")

# README 的四处版本语境：中英各两处，每处在全文必须恰好命中 1 次。
# 前两条是页首状态行，后两条在「版本方案」/「Versioning」节——同一版号要在一个
# 文档的两端都写对，只改一处就会被抓出来。
# 命中值：与真源完全相等 → 过；页首那两处若是 X.Y.Z-beta.x 占位 → 比对主版本号。
README_PATTERNS = [
    ("README 页首「**当前版本**：`…`」", re.compile(r"\*\*当前版本\*\*：`([^`]+)`")),
    ("README 版本方案「当前为 `…`」", re.compile(r"当前为 `(" + VER + r")`")),
    ("README 页首「**Current version**: `…`」", re.compile(r"\*\*Current version\*\*: `([^`]+)`")),
    ("README Versioning「the current release is `…`」", re.compile(r"the current release is\s+`(" + VER + r")`")),
]

CHANGELOG_FIRST = re.compile(r"^## v(" + VER + r")", re.MULTILINE)


def read(rel: str) -> str:
    return (ROOT / rel).read_text(encoding="utf-8")


def cargo_toml_version() -> str:
    m = re.search(r'^version\s*=\s*"([^"]+)"', read("Cargo.toml"), re.MULTILINE)
    if not m:
        raise SystemExit("check_version: Cargo.toml 里找不到 version = \"…\"")
    return m.group(1)


def main() -> int:
    problems: list[str] = []
    cur = cargo_toml_version()
    cur_base = cur.split("-")[0]

    # ---- Cargo.lock：四个成员逐一比对 ----
    lock = read("Cargo.lock")
    for member in LOCK_MEMBERS:
        m = re.search(r'name = "' + member + r'"\nversion = "' + VER + r'"', lock)
        if not m:
            problems.append(f"Cargo.lock：找不到成员 {member} 的 name/version 块")
            continue
        got = re.search(r'version = "([^"]+)"', m.group(0)).group(1)
        if got != cur:
            problems.append(f"Cargo.lock：{member} = {got}，真源是 {cur}")

    # ---- README：四个语境各恰好 1 处 ----
    readme = read("README.md")
    readme_lines = readme.split("\n")
    for label, pat in README_PATTERNS:
        hits = list(pat.finditer(readme))
        if len(hits) != 1:
            extra = f"，首处在第 {_line_of(readme, hits[0].start())} 行" if hits else ""
            problems.append(f"{label}：命中 {len(hits)} 处（应为 1）{extra}")
            continue
        got = hits[0].group(1)
        line_no = _line_of(readme, hits[0].start())
        if got != cur:
            if re.fullmatch(VER_BETA_X, got) and got.split("-")[0] == cur_base:
                continue  # 占位形式 0.5.0-beta.x：主版本号一致即认可
            problems.append(f"README.md:{line_no}：{label} 是 {got}，真源是 {cur}")

    # ---- CHANGELOG：第一个 ## v 标题 ----
    m = CHANGELOG_FIRST.search(read("CHANGELOG.md"))
    if not m:
        problems.append("CHANGELOG.md：找不到「## vX.Y.Z …」标题")
    elif m.group(1) != cur:
        line_no = _line_of(read("CHANGELOG.md"), m.start())
        problems.append(f"CHANGELOG.md:{line_no}：最新版本节是 v{m.group(1)}，真源是 {cur}")

    if problems:
        print("版本号漂移，以下各处与 Cargo.toml 的真源不一致：", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        print(f"\n真源：Cargo.toml version = {cur}。请同步上面列出的位置后再提交。", file=sys.stderr)
        return 1
    print(f"版本一致：{cur}（Cargo.toml / Cargo.lock×{len(LOCK_MEMBERS)} / README×4 / CHANGELOG）")
    return 0


def _line_of(text: str, offset: int) -> int:
    """字符偏移 → 1 起始行号（报错要能直接跳到行）。"""
    return text.count("\n", 0, offset) + 1


if __name__ == "__main__":
    sys.exit(main())
