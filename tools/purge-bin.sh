#!/usr/bin/env bash
# 构建之后的收尾：把本盘回收站清空。
#
# 为什么每次构建都要清（用户 2026-11-05 明确要求，别再问「要不要清」）：
# 这台机器上的删除在文件系统层被拦了一道 —— 连 `[System.IO.Directory]::Delete`
# 都不真删，而是把文件搬进 `<盘>:\$Recycle.Bin`。于是「删掉 target」只换了地方，
# 空间一点没回；几轮下来 bin 能涨到 40 万项 / 114 GB（2026-11-05 实测 E 盘
# 402,058 项 / 114.34 GB，剩余空间只剩 5 GB），链接器随即报 LNK1180/LNK1201。
# 所以「构建后清 bin」不是洁癖，是让下一次构建还能跑。
#
# 用法：
#   tools/purge-bin.sh              # 清 E:（默认）
#   tools/purge-bin.sh D:           # 清别的盘
#   AEVUM_SKIP_BIN_PURGE=1 tools/purge-bin.sh   # 跳过（CI 上通常没有这个拦截）
#
# 退出码恒为 0：清 bin 失败不该让构建判红——它只是收尾，不是判据。
#
# Why every build needs this (asked for explicitly on 2026-11-05; do not re-ask):
# deletion on this machine is intercepted at the filesystem level -- even
# `[System.IO.Directory]::Delete` does not really delete, it moves the files into
# `<drive>:\$Recycle.Bin`. So "deleting target" only moves it, and no space comes back.
# A few rounds and the bin reaches 400k items / 114 GB (measured on E: on 2026-11-05:
# 402,058 items / 114.34 GB with 5 GB free), after which the linker reports
# LNK1180/LNK1201. "Purge the bin after a build" is not tidiness -- it is what keeps the
# next build possible.
#
# Usage:
#   tools/purge-bin.sh              # purge E: (the default)
#   tools/purge-bin.sh D:           # another drive
#   AEVUM_SKIP_BIN_PURGE=1 tools/purge-bin.sh   # skip (CI usually has no interception)
#
# Always exits 0: a failed purge must not turn a build red -- it is housekeeping, not a
# criterion.
set -uo pipefail

DRIVE="${1:-E:}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ "${AEVUM_SKIP_BIN_PURGE:-0}" = "1" ]; then
    echo "---- purge-bin: 跳过（AEVUM_SKIP_BIN_PURGE=1）"
    exit 0
fi

if ! command -v powershell >/dev/null 2>&1; then
    echo "---- purge-bin: 这台机器没有 powershell，跳过（非 Windows？）"
    exit 0
fi

# 先量一眼：空 bin 直接一句话走人，不让收尾拖住每次构建。
bin_root="${DRIVE}"'\$Recycle.Bin'
if [ ! -d "$bin_root" ]; then
    echo "---- purge-bin: $DRIVE 没有回收站，跳过"
    exit 0
fi

echo "---- purge-bin: 清 $DRIVE 的回收站（这一步可能几分钟，40 万项约 7 分钟）"
powershell -NoProfile -ExecutionPolicy Bypass -File "$ROOT/tools/empty-bin.ps1" -Drive "$DRIVE"
rc=$?
if [ "$rc" -ne 0 ]; then
    echo "---- purge-bin: empty-bin.ps1 退出码 $rc（有删不掉的项目，通常是别的账户的），构建不受影响"
fi
exit 0
