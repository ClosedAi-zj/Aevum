#!/usr/bin/env python3
"""生成 GitHub Release 的标题类型与正文，并回读发布结果做字节对账。

为什么存在：以前 Release 的下载表、改动列表、兼容性说明硬编码在本机一个临时脚本里，
发一版改一次，和 CHANGELOG / Cargo.toml 是三份各自会漂移的事实源。现在把正文生成放进
仓库，唯一事实源是 Cargo.toml（版本号）与 CHANGELOG.md（本版改了什么）；tag 触发的
release.yml 调它，本机也能单独跑来验证。

用法（Python 3，Windows / Linux 都可；不依赖第三方库）：
    python scripts/release_body.py v1.9.6                 # 打印完整正文
    python scripts/release_body.py --kind                # 只打印「补丁版 / 正式版」
    python scripts/release_body.py --verify api.json dist/Aevum-Studio-v1.9.6-windows-x64.zip
        # 回读 gh api 返回的 release JSON：非 draft、tag_name 与 zip 名一致、恰好两个资产、
        # state=uploaded、zip 的 digest 必须存在且等于本地 sha256、本地 SHA256SUMS 与 zip 一致。
        # 任何一环缺失都判红，不假装验过。

Why this exists: release notes used to be hand-written in a throwaway local script, so the
download table, changelog and compatibility note drifted independently of Cargo.toml. Now the
repository owns the generator; Cargo.toml is the single source for the version and
CHANGELOG.md for the changes. release.yml calls it, and it can also be run locally to verify.

Usage (Python 3, Windows/Linux; no third-party dependencies):
    python scripts/release_body.py v1.9.6                 # print the full body
    python scripts/release_body.py --kind                # print just the release type
    python scripts/release_body.py --verify api.json dist/Aevum-Studio-v1.9.6-windows-x64.zip
        # Read back the release JSON: not a draft, tag_name matching the zip name, exactly two
        # assets with state=uploaded, a digest that must exist and equal the local sha256, and a
        # local SHA256SUMS that agrees with the zip. A missing link fails instead of faking a pass.
"""

from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
VER = r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?"


def read(rel: str) -> str:
    return (ROOT / rel).read_text(encoding="utf-8")


def version() -> str:
    m = re.search(r'^version\s*=\s*"([^"]+)"', read("Cargo.toml"), re.MULTILINE)
    if not m:
        raise SystemExit("release_body: Cargo.toml 里找不到 version")
    return m.group(1)


def kind_of(ver: str) -> str:
    """第三段非 0 是补丁版，否则是正式版。标题类型由版本号推，不靠人手填。"""
    patch = int(ver.split(".")[2].split("-")[0])
    return "补丁版" if patch else "正式版"


def section(ver: str) -> str:
    """取 CHANGELOG.md 最新一节正文；首节标题必须与真源版本号一致。"""
    text = read("CHANGELOG.md")
    first = re.search(r"^## v(" + VER + r")", text, re.MULTILINE)
    if not first or first.group(1) != ver:
        raise SystemExit(
            f"release_body: CHANGELOG.md 的首节是 v{first.group(1) if first else '?'}，"
            f"Cargo.toml 是 {ver} —— 先补变更记录再发版"
        )
    m = re.search(
        r"^## v" + re.escape(ver) + r"[^\n]*\n(.*?)(?=^---\s*$|^## v|\Z)",
        text,
        re.MULTILINE | re.DOTALL,
    )
    if not m or not m.group(1).strip():
        raise SystemExit(f"release_body: CHANGELOG.md 里 v{ver} 这一节是空的")
    return m.group(1).strip()


def body(tag: str) -> str:
    ver = version()
    if tag != "v" + ver:
        raise SystemExit(f"release_body: tag {tag} 与 Cargo.toml 的 v{ver} 不一致")
    zip_name = f"Aevum-Studio-v{ver}-windows-x64.zip"
    return (
        "## 下载\n\n"
        "| 文件 | 说明 |\n"
        "| --- | --- |\n"
        f"| `{zip_name}` | Windows x64 绿色包：解压即用，不写注册表、不需要管理员权限 |\n"
        "| `SHA256SUMS` | 校验和；下载后在同一目录运行 `sha256sum -c SHA256SUMS` |\n\n"
        f"{section(ver)}\n\n"
        "---\n\n"
        "兼容性：1.x 的四类对外契约（工程文件格式、预设与参数持久化、命令行参数、"
        "MCP 工具集）按「只增不改」维护，详见 README「版本方案」。"
        "本包未做代码签名，首次运行时 SmartScreen 的「未知发布者」提示属预期。\n"
    )


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def verify(api_json: Path, zip_path: Path) -> None:
    """发布不是「gh 说成功」就算完：回读 API，确认非 draft、tag 一致、资产齐、state 与 digest 对得上。

    A successful `gh release create` is not proof: read the API back and check that the
    release is not a draft, the tag matches the zip, both assets are present with
    state=uploaded, the digest exists and matches the local bytes, and SHA256SUMS agrees.
    """
    data = json.loads(api_json.read_text(encoding="utf-8"))
    if data.get("draft"):
        raise SystemExit("release_body: Release 还是 draft")

    # 从 zip 名反推期望 tag，防「回读到别的 Release 的 JSON 却通过」。
    # 后缀必须锚住 -windows-x64.zip：否则可选的预发布段会把 "-windows" 也吃进去，
    # 把 v1.9.6 误读成 v1.9.6-windows。
    # Derive the expected tag from the zip name so a JSON from another Release cannot pass.
    # The suffix has to be anchored: without it the optional pre-release segment swallows
    # "-windows" and turns v1.9.6 into v1.9.6-windows.
    match = re.search(
        r"(v\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?)-windows-x64\.zip$", zip_path.name
    )
    if not match:
        raise SystemExit(f"release_body: 从 {zip_path.name} 里读不出期望的 tag")
    expected_tag = match.group(1)
    if data.get("tag_name") != expected_tag:
        raise SystemExit(
            f"release_body: 回读的是 tag={data.get('tag_name')}，期望 {expected_tag}"
        )

    assets = {a["name"]: a for a in data.get("assets", [])}
    zip_name = zip_path.name
    if set(assets) != {zip_name, "SHA256SUMS"}:
        raise SystemExit(f"release_body: 资产清单不对：{sorted(assets)}")
    for name, asset in assets.items():
        if asset.get("state") != "uploaded":
            raise SystemExit(f"release_body: {name} 的 state 是 {asset.get('state')}")

    # GitHub API 现在确实返回 digest（v1.9.6 两资产均已实证）：缺了就是没验过，判红而不是跳过。
    # The API does return a digest now (proven on both v1.9.6 assets); a missing one means
    # "not verified", which must fail rather than be reported as a pass.
    digest = assets[zip_name].get("digest")
    if not digest:
        raise SystemExit("release_body: API 未返回 digest，无法确认分发出去的字节与本地一致")
    local = sha256(zip_path)
    if digest != "sha256:" + local:
        raise SystemExit(f"release_body: {zip_name} 的 digest 与本地 sha256 不一致")
    checked = f"digest 对账一致（{local[:16]}…）"

    sums = (zip_path.parent / "SHA256SUMS").read_text(encoding="utf-8").strip()
    if not sums.startswith(local) or zip_name not in sums:
        raise SystemExit("release_body: 本地 SHA256SUMS 与 zip 的 sha256 不一致")

    print(
        f"Release 回读：tag={data.get('tag_name')} draft=false "
        f"assets={len(assets)} state=uploaded {checked}"
    )


def _utf8_streams() -> None:
    """英文 Windows 的 stdout 默认 cp1252，打印中文会直接抛异常（CI 连红四次那次）。"""
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="replace")


def main() -> int:
    args = sys.argv[1:]
    if args == ["--kind"]:
        print(kind_of(version()))
    elif len(args) == 3 and args[0] == "--verify":
        verify(Path(args[1]), Path(args[2]))
    elif len(args) == 1 and args[0].startswith("v"):
        sys.stdout.write(body(args[0]))
    else:
        print(__doc__, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    _utf8_streams()
    sys.exit(main())
