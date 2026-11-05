#!/usr/bin/env python3
"""Release 回读 `verify()` 失败分支的可回归自测（acceptance/check.sh 第 0 关）。

中文：v1.9.6 发布时，`scripts/release_body.py --verify` 的四种失败（draft / digest 不符 /
缺资产 / 未传完）是用**一次性人工假 JSON** 验过的，验完就散在聊天记录里；后来改了
`verify()` 也没人能重跑。这份脚本把那次人工验证固化成可回归资产：用 `tempfile` 造一份
假 zip、同目录 `SHA256SUMS` 与 GitHub 形状的 release JSON，通过 `subprocess` 调真正的
`release_body.py --verify`，覆盖八个场景（含「API 未返回 digest」与「tag_name 不符」两条
当时弱失败/漏掉的分支）。无网络、无 cargo、无第三方依赖，纯标准库。

English: at the v1.9.6 release the four `--verify` failure branches (draft / digest mismatch /
missing asset / not uploaded) were checked once by hand with a throwaway fake JSON and then
lived only in a chat log -- nobody could re-run them after `verify()` changed. This script is
the regression version of that one-off verification: `tempfile` builds a fake zip, a sibling
`SHA256SUMS` and a GitHub-shaped release JSON, and `subprocess` calls the real
`release_body.py --verify` for eight scenarios, including the two branches that were weak or
missing at the time (API returns no digest; wrong tag_name). No network, no cargo, no
third-party dependencies -- standard library only.

zip 文件名里的版本号从 `Cargo.toml` 读出，不写死；`verify()` 只对 zip 做 sha256，
所以这里用固定字节造一份真 zip 形状即可，不需要可解压的发行包。

The zip name is built from the version in `Cargo.toml`, never hardcoded; `verify()` only
hashes the zip bytes, so a deterministic fake zip is enough (no real release payload needed).

自举 / Bootstrap: after dropping `#` comment lines, `acceptance/check.sh` must still contain
a real call to `acceptance/test_release_body.py` (same technique as R9 in check_release.py).
"""

from __future__ import annotations

import hashlib
import io
import json
import re
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path
from typing import NamedTuple

ROOT = Path(__file__).resolve().parents[1]
BODY = "scripts/release_body.py"
GATE = "acceptance/check.sh"
SELF_CALL = "acceptance/test_release_body.py"
# 只认「python 真调了一行」，光把文件名写进 echo 不算数。
# Only a real `python ...` invocation counts; echoing the filename does not.
SELF_CALL_RE = re.compile(
    r"^\s*python\s+.*acceptance/test_release_body\.py", re.MULTILINE
)


class Case(NamedTuple):
    """一个回读场景：名字、release JSON、SHA256SUMS 内容、期望结果、失败关键词。

    One read-back scenario: name, release JSON, SHA256SUMS text, expected outcome,
    and a keyword the failure output must contain (`None` means only non-zero matters).
    """

    name: str
    payload: dict
    sums_text: str
    want_ok: bool
    keyword: str | None


def current_version() -> str:
    """从 Cargo.toml 读当前版本号；换版本时这份自测不用改。

    Read the current version from Cargo.toml so the self-test survives the next release.
    """
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'^version\s*=\s*"([^"]+)"', text, re.MULTILINE)
    if not match:
        raise SystemExit("test_release_body: Cargo.toml 里找不到 version")
    return match.group(1)


def fake_zip_bytes() -> bytes:
    """固定时间戳造一份真 zip 的字节；每次写盘都是同一串，sha256 才可预测。

    Build a real (tiny) zip with a fixed timestamp so its sha256 is stable across cases.
    """
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        members = (
            ("aevum.exe", b"Aevum fake release payload for verify() self-test\n"),
            ("tools/ffmpeg.exe", b"fake ffmpeg\n"),
        )
        for name, data in members:
            info = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, data)
    return buffer.getvalue()


def base_payload(zip_name: str, digest: str, tag: str) -> dict:
    """GitHub API 形状的最小 release JSON：非 draft、两资产都 uploaded、digest 正确。

    A minimal GitHub-API-shaped release JSON: not a draft, both assets uploaded,
    and the zip digest matching the local bytes.
    """
    return {
        "tag_name": tag,
        "draft": False,
        "assets": [
            {"name": zip_name, "state": "uploaded", "digest": "sha256:" + digest},
            {
                "name": "SHA256SUMS",
                "state": "uploaded",
                "digest": "sha256:" + hashlib.sha256(b"SHA256SUMS asset placeholder").hexdigest(),
            },
        ],
    }


def build_cases(zip_name: str, digest: str, tag: str) -> list[Case]:
    """八个场景：一条正常，七条各自只破坏一个条件。

    Eight scenarios: one happy path and seven that each break exactly one condition.
    """
    good_sums = f"{digest}  {zip_name}\n"
    bad_sums = "0" * 64 + f"  {zip_name}\n"
    cases: list[Case] = []

    cases.append(Case("正常", base_payload(zip_name, digest, tag), good_sums, True, None))

    draft = base_payload(zip_name, digest, tag)
    draft["draft"] = True
    cases.append(Case("draft=true", draft, good_sums, False, "draft"))

    missing = base_payload(zip_name, digest, tag)
    missing["assets"] = [a for a in missing["assets"] if a["name"] != "SHA256SUMS"]
    cases.append(Case("资产清单缺 SHA256SUMS", missing, good_sums, False, "资产清单"))

    not_uploaded = base_payload(zip_name, digest, tag)
    not_uploaded["assets"][1]["state"] = "pending"
    cases.append(Case("某资产 state != uploaded", not_uploaded, good_sums, False, "state"))

    wrong_digest = base_payload(zip_name, digest, tag)
    wrong_digest["assets"][0]["digest"] = "sha256:" + "0" * 64
    cases.append(Case("zip digest 不符", wrong_digest, good_sums, False, "digest"))

    cases.append(
        Case(
            "SHA256SUMS 内容与 zip 不符",
            base_payload(zip_name, digest, tag),
            bad_sums,
            False,
            "SHA256SUMS",
        )
    )

    no_digest = base_payload(zip_name, digest, tag)
    del no_digest["assets"][0]["digest"]
    cases.append(Case("API 未返回 digest", no_digest, good_sums, False, None))

    wrong_tag = base_payload(zip_name, digest, tag)
    wrong_tag["tag_name"] = "v0.0.0"
    cases.append(Case("tag_name 不符", wrong_tag, good_sums, False, "tag"))

    return cases


def run_case(case: Case, zip_name: str, data: bytes) -> bool:
    """落一份临时发行目录，调真正的 `release_body.py --verify`，核对退出码与关键词。

    Materialise a temporary release directory, call the real `release_body.py --verify`,
    and check both the exit code and (for failures) the named keyword.
    """
    with tempfile.TemporaryDirectory(prefix="aevum-release-body-") as tmp:
        tmp_path = Path(tmp)
        zip_path = tmp_path / zip_name
        zip_path.write_bytes(data)
        (tmp_path / "SHA256SUMS").write_text(case.sums_text, encoding="utf-8")
        json_path = tmp_path / "release.json"
        json_path.write_text(
            json.dumps(case.payload, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        proc = subprocess.run(
            [
                sys.executable,
                str(ROOT / BODY),
                "--verify",
                str(json_path),
                str(zip_path),
            ],
            cwd=ROOT,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
    output = proc.stdout + proc.stderr
    if case.want_ok:
        ok = proc.returncode == 0
    else:
        ok = proc.returncode != 0 and (
            case.keyword is None or case.keyword in output
        )
    print(f"{case.name} {'✓' if ok else '✗'}")
    if not ok:
        print(
            f"    exit={proc.returncode}，输出：{output.strip()[:280]}",
            file=sys.stderr,
        )
    return ok


def bootstrap_problem() -> str | None:
    """自举：剔除注释行后，check.sh 里必须还有对本自测的真实调用。

    Bootstrap: after comment lines are removed, check.sh must still call this self-test.
    """
    gate = ROOT / GATE
    if not gate.is_file():
        return f"test_release_body: 缺少 {GATE}，无法确认本自测真的挂在门槛上"
    real_lines = "\n".join(
        line
        for line in gate.read_text(encoding="utf-8").splitlines()
        if not line.lstrip().startswith("#")
    )
    if not SELF_CALL_RE.search(real_lines):
        return (
            f"test_release_body: 自举判据失败 —— {GATE} 第 0 关没有真实调用 {SELF_CALL}"
            "（注释行与 echo 不算，写了不跑等于没写）"
        )
    return None


def main() -> int:
    version = current_version()
    zip_name = f"Aevum-Studio-v{version}-windows-x64.zip"
    tag = f"v{version}"
    data = fake_zip_bytes()
    digest = hashlib.sha256(data).hexdigest()

    boot = bootstrap_problem()
    if boot is not None:
        print(boot, file=sys.stderr)
        return 1

    results = [run_case(case, zip_name, data) for case in build_cases(zip_name, digest, tag)]
    if not all(results):
        print(
            "发行回读自测：有场景没按预期红/绿（见上面的 ✗）/ "
            "Some read-back scenarios did not behave as expected",
            file=sys.stderr,
        )
        return 1
    print(
        f"发行回读自测：8 个场景全过（版本 {version} 由 Cargo.toml 读出）/ "
        "All eight read-back scenarios passed"
    )
    return 0


def _utf8_streams() -> None:
    """把 stdout/stderr 掰成 UTF-8：英文 Windows 默认 cp1252，打印中文会抛异常。

    Pin stdout/stderr to UTF-8: English Windows defaults to cp1252, so a Chinese
    scenario name (or a ✓/✗) would raise UnicodeEncodeError instead of reporting.
    """
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="replace")


if __name__ == "__main__":
    _utf8_streams()
    sys.exit(main())
