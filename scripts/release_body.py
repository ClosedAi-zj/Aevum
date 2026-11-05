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
        # 回读 gh api 返回的 release JSON：非 draft、两个资产、state=uploaded，
        # 且 zip 资产的 digest 与本地文件 sha256 逐字节相等。

Why this exists: release notes used to be hand-written in a throwaway local script, so the
download table, changelog and compatibility note drifted independently of Cargo.toml. Now the
repository owns the generator; Cargo.toml is the single source for the version and
CHANGELOG.md for the changes. release.yml calls it, and it can also be run locally to verify.
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
    """发布不是「gh 说成功」就算完：回读 API，确认非 draft、资产齐、digest 对得上。"""
    data = json.loads(api_json.read_text(encoding="utf-8"))
    if data.get("draft"):
        raise SystemExit("release_body: Release 还是 draft")
    assets = {a["name"]: a for a in data.get("assets", [])}
    zip_name = zip_path.name
    if set(assets) != {zip_name, "SHA256SUMS"}:
        raise SystemExit(f"release_body: 资产清单不对：{sorted(assets)}")
    for name, asset in assets.items():
        if asset.get("state") != "uploaded":
            raise SystemExit(f"release_body: {name} 的 state 是 {asset.get('state')}")

    digest = assets[zip_name].get("digest")
    local = sha256(zip_path)
    if digest:
        if digest != "sha256:" + local:
            raise SystemExit(f"release_body: {zip_name} 的 digest 与本地 sha256 不一致")
        checked = f"digest 对账一致（{local[:16]}…）"
    else:
        # 老版本 API / 某些资产类型不返回 digest。不假装验过，明说这一环缺了。
        checked = "API 未返回 digest，跳过字节对账"

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
