#!/usr/bin/env python3
"""文档双语一致性检查（check.sh 第 0 关的后半段）。

为什么存在：用户的要求是「文档都要中英两种语言，**点击可以跳转**，而不是一个文件
两种语言」。中英分册之后最容易出的事不是翻译质量，而是三种**静默失效**：

  1. 新增一份中文文档，忘了写英文孪生——英文读者点进来只有中文；
  2. 孪生写了，顶部忘了切换链接——对方永远找不到另一册（「点击跳转」就落空了）；
  3. 链接写错文件名/目录（中英文档在 docs/ 里，链接却按仓库根写）——跳转是死链。

三种都不会让构建失败、也不会被任何测试发现，只有读者点下去才知道。所以放进门槛，
和版本号一起查：文档是随 tag 走的对外承诺，漂移了就该在提交时红。

检查口径：
  1. 入库的每一份 `*.md` 必须有 `.en.md` 孪生，且 `.en.md` 也必须有中文孪生
     （双向：只放一份英文孤儿文件同样是半套文档）；
  2. 每份文档**前 15 行**里必须有指向孪生的链接（`**中文** · [English](x.en.md)`
     或反向），且链接目标真实存在；
  3. 文档里所有**本地相对链接**都能落到真实文件（http/mailto/纯锚点除外）。
     站外链接不查——网络不是提交门槛能控制的东西。

豁免：`docs/` 下未入库的过程文档（`.gitignore` 里忽略的那些）不在检查范围，
本脚本只认 `git ls-files` 的结果，所以「有没有入库」本身就是口径。
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parents[1]

# 切换链接必须出现在前几行：读者是「打开就找语言」，翻到文末才看到等于没有。
SWITCH_WINDOW = 15

# [text](target) —— 只取圆括号里第一个非空片段，忽略可选的 "title"。
MD_LINK = re.compile(r"\[[^\]]*\]\(\s*<?([^)\s>]+)>?(?:\s+\"[^\"]*\")?\s*\)")


def tracked_md() -> list[str]:
    """入库的 markdown ∪ 已新建但还没 `git add` 的 markdown。

    只认 `git ls-files` 会让「刚写好英文册、还没 add」被判成缺孪生，跑一次红一次、
    白跑一趟；补上 `-o --exclude-standard`（未跟踪且未被忽略）就与提交时的实际内容一致。
    `.gitignore` 忽略的过程文档仍然不在范围内。
    """
    out = subprocess.run(
        ["git", "-c", "core.quotepath=false", "ls-files", "-z", "-co", "--exclude-standard", "*.md"],
        cwd=ROOT,
        capture_output=True,
        check=True,
    ).stdout
    files = {p.decode("utf-8") for p in out.split(b"\0") if p}
    # 删除但未提交的文件会出现在 `ls-files -c` 里：只保留磁盘上真实存在的。
    return sorted(f for f in files if (ROOT / f).is_file())


def twin_of(rel: str) -> str:
    """中文册 ↔ 英文册的孪生路径（两个方向都要能用）。"""
    if rel.endswith(".en.md"):
        return rel[: -len(".en.md")] + ".md"
    return rel[:-len(".md")] + ".en.md"


def line_of(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def check_pair(rel: str, existing: set[str]) -> list[str]:
    problems: list[str] = []
    twin = twin_of(rel)
    if twin not in existing:
        kind = "中文孪生" if rel.endswith(".en.md") else "英文孪生"
        return [f"{rel}：没有{kind} {twin}（中英分册要求两份都在，孤儿文件同样是半套）"]
    text = (ROOT / rel).read_text(encoding="utf-8")
    head = "\n".join(text.split("\n")[:SWITCH_WINDOW])
    want = Path(twin).name
    linked = [t for t in MD_LINK.findall(head) if unquote(Path(t).name) == want]
    if not linked:
        problems.append(
            f"{rel}：前 {SWITCH_WINDOW} 行没有指向 {want} 的语言切换链接"
            "（读者找不到另一册，等于只有单语）"
        )
    return problems


def check_links(rel: str) -> list[str]:
    text = (ROOT / rel).read_text(encoding="utf-8")
    problems: list[str] = []
    fence = re.compile(r"```.*?```", re.S)
    spans = [(m.start(), m.end()) for m in fence.finditer(text)]
    # 行内反引号里也会有 `[English](x.en.md)` 这种占位示例（讲版式的那几句就是），
    # 它们不是真链接，一并跳过。
    spans += [(m.start(), m.end()) for m in re.finditer(r"`[^`\n]*`", text)]
    for m in MD_LINK.finditer(text):
        # 代码块里的示例链接（`[中文](x.md)` 这种）不是真链接，跳过。
        if any(a <= m.start() < b for a, b in spans):
            continue
        target = m.group(1)
        if target.startswith(("http://", "https://", "mailto:", "#", "<")):
            continue
        path = unquote(target.split("#", 1)[0])
        if not path:
            continue
        resolved = (ROOT / rel).parent / path
        if not resolved.exists():
            problems.append(
                f"{rel}:{line_of(text, m.start())}：链接目标不存在 → {target}"
                f"（解析为 {resolved.relative_to(ROOT).as_posix()}）"
            )
    return problems


def main() -> int:
    files = tracked_md()
    if not files:
        print("check_docs: 没有扫到入库的 markdown，仓库根或 git 状态有问题", file=sys.stderr)
        return 1
    existing = set(files)
    problems: list[str] = []
    # 双语互查走全量而不是只查中文册：英文册忘了反向链接，中文读者同样找不到它。
    for rel in files:
        problems += check_pair(rel, existing)
    for rel in files:
        problems += check_links(rel)
    pairs = len([f for f in files if not f.endswith(".en.md")])

    if problems:
        print("文档双语/链接有问题：", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        print(
            f"\n共 {len(files)} 份入库文档，{pairs} 对中英齐备。"
            "补法见 acceptance/README.md「文档」一节：中文在前、顶部互相链接。",
            file=sys.stderr,
        )
        return 1

    print(f"文档双语完整：{pairs} 对中英互链，{len(files)} 份文档的本地链接全部可达")
    return 0


def _utf8_streams() -> None:
    """把 stdout/stderr 掰成 UTF-8，别让终端语言决定能不能打印。

    英文 Windows 上 Python 默认按 cp1252 输出，而本脚本的结论句是中文：第一个
    print 就抛 UnicodeEncodeError，检查还没做完就被判成「脚本崩了」。errors 取
    replace 是最后一道保险——编码对了它不起作用，遇到意料之外的字符也只是显示成
    问号，不会把结论变成一次崩溃。
    """
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="replace")


if __name__ == "__main__":
    _utf8_streams()
    sys.exit(main())
