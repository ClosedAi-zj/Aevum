#!/usr/bin/env bash
# 提交门槛：格式 → clippy（警告即失败）→ 全量测试 → 界面布局自检。
#
# 用法（Git Bash）：
#   scripts/check.sh                    # 本机默认离线，用已缓存的依赖
#   CARGO_FLAGS= scripts/check.sh       # CI 上允许联网解析依赖
#
# 四步任何一步红就停，别把脏改动带进 package.sh。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# 本机 gitconfig 里有死代理，cargo 必须临时清掉代理变量。
CARGO=(env -u http_proxy -u https_proxy -u HTTP_PROXY -u HTTPS_PROXY \
    -u all_proxy -u ALL_PROXY CARGO_HTTP_PROXY= cargo)
# 只有「没设」才回落成本机离线；显式设成空串是 CI 的「允许联网解析」，
# 用 `:-` 会把空值也当成未设置，CI 上就变成没缓存还强制 offline。
CARGO_FLAGS="${CARGO_FLAGS---offline}"

echo "==> 1/4 格式"
"${CARGO[@]}" fmt --all -- --check

echo "==> 2/4 clippy（-D warnings）"
"${CARGO[@]}" clippy --workspace --all-targets $CARGO_FLAGS -- -D warnings

echo "==> 3/4 全量测试（含与 Python 基准的金标比对）"
"${CARGO[@]}" test --workspace $CARGO_FLAGS

echo "==> 4/4 界面布局自检（无显示器）"
"${CARGO[@]}" build -p aevum-studio $CARGO_FLAGS
exe="$ROOT/target/debug/aevum.exe"
[ -f "$exe" ] || exe="$ROOT/target/debug/aevum-studio"
"$exe" --selftest

echo "==> 门槛全绿"
