#!/usr/bin/env python3
"""发行链静态契约（check.sh 第 0 关）。

为什么存在：release 包以前完全依赖一台机器，CI 只跑六关。把打包搬进 CI 之后，最容易
悄悄退化的是「CI 与本机跑的是不是同一条链」——有人在 workflow 里重抄一遍 cargo build
和 zip 命令、换成一个 latest 的 ffmpeg、把 --offline 写死、或者让 workflow_dispatch 也
发 Release，本地六关都不会红。这些是**配置腐蚀**，不是编译错误，只能靠静态契约盯住。

口径：只读入库文本，不联网、不跑 cargo、不依赖 runner 环境，所以干净 clone 与英文
Windows 上同样成立。注释行（行首 #）不参与匹配——否则判据自己写的说明会把自己绊倒。

每条判据都有编号 R1..R9，改动 release 链时请逐条做变异验证（改一处 → 本脚本必须红并
点名那个编号），别只跑一遍「绿」。

Why this exists: release artifacts used to depend on one machine and CI only ran the six
gates. Once packaging moved to CI, the silent failure mode is a workflow that stops being
the same chain as local builds: a copied cargo build, a floating ffmpeg URL, a hard-coded
--offline, or a dispatch run that publishes a Release. None of that fails compilation.
These are read-only text contracts; comment lines are ignored so the checks cannot trip
over their own documentation.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

WF = ".github/workflows/release.yml"
PACKAGE = "scripts/package.sh"
GATE = "acceptance/check.sh"
BODY = "scripts/release_body.py"
PURGE = "tools/purge-bin.sh"
IGNORE = ".gitignore"


def read(rel: str) -> str:
    path = ROOT / rel
    if not path.is_file():
        raise SystemExit(f"check_release: 缺少 {rel} —— 发行链文件被删了")
    return path.read_text(encoding="utf-8")


def lines(text: str) -> list[str]:
    """有效行：去掉纯注释行（行首空白后是 #）。"""
    return [ln for ln in text.splitlines() if not ln.lstrip().startswith("#")]


def has(text: str, pattern: str) -> bool:
    return any(re.search(pattern, ln) for ln in lines(text))


def check(wf: str, pkg: str, gate: str, body: str, purge: str, ignore: str) -> list[str]:
    problems: list[str] = []

    # R1：tag 触发 + 手动验证入口 + 发 Release 需要的写权限。
    if not (
        has(wf, r'^\s+tags:\s*\["v\*"\]')
        and has(wf, r"^\s*workflow_dispatch:\s*$")
        and has(wf, r"^\s*contents:\s*write\s*$")
    ):
        problems.append("R1 触发条件/权限：release.yml 必须 tags: [\"v*\"] + workflow_dispatch + contents: write")

    # R2：随包 ffmpeg 固定来源、固定字节，且真的做了校验并交给打包脚本。
    url = next((ln for ln in lines(wf) if "FFMPEG_URL:" in ln), "")
    hashline = next((ln for ln in lines(wf) if "FFMPEG_EXE_SHA256:" in ln), "")
    if not (
        "/releases/download/" in url
        and not re.search(r"latest|master|autobuild", url)
        and re.search(r"[0-9a-f]{64}", hashline)
        and has(wf, r'^\s*FFMPEG_ZIP_SIZE:\s*"\d+"')
        and has(wf, r"sha256sum -c -")
        and has(wf, r"AEVUM_FFMPEG_SRC=")
    ):
        problems.append("R2 ffmpeg：固定 tag URL + 64 位 exe sha256 + zip 大小 + sha256sum -c + AEVUM_FFMPEG_SRC")

    # R3：本机离线 / CI 联网是同一个变量，release 构建不许写死 --offline。
    if not (
        has(wf, r'^\s*CARGO_FLAGS:\s*""')
        and has(pkg, r'CARGO_FLAGS="\$\{CARGO_FLAGS---offline\}"')
        and has(pkg, r"build -p aevum-studio --release \$CARGO_FLAGS")
        and not has(pkg, r"--release --offline")
        and has(gate, r'CARGO_FLAGS="\$\{CARGO_FLAGS---offline\}"')
    ):
        problems.append("R3 离线开关：CARGO_FLAGS 在 workflow / package.sh / check.sh 三处必须同源，release 构建不写死 --offline")

    # R4：runner 没有本机的「删除被拦进回收站」钩子，清 bin 必须能跳过。
    if not (
        has(wf, r'^\s*AEVUM_SKIP_BIN_PURGE:\s*"1"')
        and has(purge, r'AEVUM_SKIP_BIN_PURGE:-\.?0?.*= "1"')
    ):
        problems.append("R4 回收站：workflow 必须设 AEVUM_SKIP_BIN_PURGE=1，且 purge-bin.sh 自己认这个开关")

    # R5：CI 跑的是同一个脚本，且产物自己验过校验和。
    if not (
        has(wf, r"bash scripts/package\.sh")
        and not has(wf, r"cargo build|tar\.exe -a -c")
        and has(wf, r"sha256sum -c SHA256SUMS")
        and has(wf, r"licenses/LGPL-2\.1-rawler\.txt")
        and has(wf, r"third-party-src/rawler-\*\.crate")
    ):
        problems.append("R5 单一打包链：必须调 scripts/package.sh、不重抄构建/压缩步骤，并验 SHA256SUMS 与 LGPL 义务文件")

    # R6：两个资产上传；只有 tag 才发 Release；发布后回读对账。
    if not (
        has(wf, r"actions/upload-artifact@v4")
        and has(wf, r"dist/\*\.zip")
        and has(wf, r"dist/SHA256SUMS")
        and has(wf, r"if: startsWith\(github\.ref, 'refs/tags/v'\)")
        and has(wf, r"gh release create")
        and has(wf, r"\$RELEASE_ZIP")
        and has(wf, r"release_body\.py --verify(?:\s|$)")
    ):
        problems.append("R6 发布：zip + SHA256SUMS 两资产、tag gate、gh release create、发布后 --verify 回读")

    # R7：正文来自仓库里的真源，不在 workflow 里手写改动列表。
    if not (
        has(wf, r"scripts/release_body\.py")
        and has(body, r'read\("CHANGELOG\.md"\)')
        and has(body, r'read\("Cargo\.toml"\)')
        and has(body, r"def verify\(")
        and has(body, r"hashlib\.sha256")
    ):
        problems.append("R7 Release 正文：必须由 scripts/release_body.py 从 CHANGELOG/Cargo.toml 生成，并带字节对账")

    # R8：87 MB 的 ffmpeg.exe 不能进版本库，CI 也不能把产物提交回去。
    if not (
        has(ignore, r"^dist/\s*$")
        and not has(wf, r"git add|git commit")
    ):
        problems.append("R8 仓库边界：dist/ 必须被忽略，workflow 不许把构建产物提交回仓库")

    # R9：自举——这道契约必须真的挂在第 0 关，写了不跑等于没写。
    if not has(gate, r"acceptance/check_release\.py"):
        problems.append("R9 自举：acceptance/check.sh 第 0 关没有调用 check_release.py")

    return problems


def _utf8_streams() -> None:
    """英文 Windows 的 stdout 默认 cp1252，打印中文会直接抛异常。"""
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="replace")


def main() -> int:
    problems = check(
        read(WF), read(PACKAGE), read(GATE), read(BODY), read(PURGE), read(IGNORE)
    )
    if problems:
        print("发行链静态契约不通过：", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        return 1
    print("发行链静态契约：R1-R9 全过（触发/ffmpeg 固定/离线开关/回收站/单一打包链/发布/正文/仓库边界/自举）")
    return 0


if __name__ == "__main__":
    _utf8_streams()
    sys.exit(main())
