#!/usr/bin/env bash
# 提交门槛：版本一致性 → 格式 → clippy（警告即失败）→ 全量测试 → 界面布局自检。
#
# 用法（Git Bash）：
#   acceptance/check.sh                 # 本机默认离线，用已缓存的依赖
#   CARGO_FLAGS= acceptance/check.sh    # CI 上允许联网解析依赖
#
# 五步任何一步红就停，别把脏改动带进 scripts/package.sh。
set -euo pipefail

# 本脚本在 acceptance/ 下，上一层才是仓库根（cargo 必须在根上跑）。
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# 本机 gitconfig 里有死代理，cargo 必须临时清掉代理变量。
# CARGO_INCREMENTAL=0：门槛会重编整个 workspace，增量缓存对本机没好处，却会
# 在 target/debug/incremental 里堆出几十 GB（实测一次 20.5 GB / 35k 文件，
# 还伴随「删除 lock 文件被拒绝」的告警）。开发时的日常增量编译不受影响。
CARGO=(env -u http_proxy -u https_proxy -u HTTP_PROXY -u HTTPS_PROXY \
    -u all_proxy -u ALL_PROXY CARGO_HTTP_PROXY= CARGO_INCREMENTAL=0 cargo)
# 只有「没设」才回落成本机离线；显式设成空串是 CI 的「允许联网解析」，
# 用 `:-` 会把空值也当成未设置，CI 上就变成没缓存还强制 offline。
CARGO_FLAGS="${CARGO_FLAGS---offline}"

# 第 0 关：版本号一致性（1.0.0 验收清单 B5，beta.2 当天就漂移过一次 README）
# + 文档双语一致性（中英分册之后：缺孪生、缺切换链接、死链都是静默失效）
# + 代码注释双语（模块级 //! 与 pub 项的 /// 必须有英文块）。
# 只读文件、不碰 cargo，所以 CARGO_FLAGS（CI 联网 / 本机 --offline）对它无影响。
echo "==> 0/5 版本与文档一致性（Cargo.toml 真源 → lock / README / CHANGELOG；中英孪生；注释双语）"
python "$ROOT/acceptance/check_version.py"
python "$ROOT/acceptance/check_docs.py"
python "$ROOT/tools/doc_lint.py"

echo "==> 1/5 格式"
"${CARGO[@]}" fmt --all -- --check

echo "==> 2/5 clippy（-D warnings）"
"${CARGO[@]}" clippy --workspace --all-targets $CARGO_FLAGS -- -D warnings

echo "==> 3/5 全量测试（含与 Python 基准的金标比对）"
# 导出类用例需要 ffmpeg，**没有就是红**（它们以前是「找不到就跳过」，于是 12 个用例
# 在本机与 CI 上都没跑过，AVIF 导出坏了一整轮没人知道）。探针顺序按「用户实际拿到
# 的那个」优先：包内 ffmpeg（dist/aevum/tools，与发行包逐字节相同）→ AEVUM_FFMPEG
# → PATH。三者都没有就让测试自己红，不在这里偷偷放行。
if [ -z "${AEVUM_FFMPEG:-}" ]; then
    for cand in "$ROOT/dist/aevum/tools/ffmpeg.exe" "$(command -v ffmpeg || true)"; do
        if [ -n "$cand" ] && [ -f "$cand" ]; then
            export AEVUM_FFMPEG="$cand"
            break
        fi
    done
fi
if [ -n "${AEVUM_FFMPEG:-}" ]; then
    echo "    ffmpeg：$AEVUM_FFMPEG"
else
    echo "    ⚠ 没有 ffmpeg：导出类用例会红。装一个、设 AEVUM_FFMPEG，或显式声明"
    echo "      AEVUM_ALLOW_NO_FFMPEG=1 跳过（跳过会喊出来，别指望它算通过）。"
fi
"${CARGO[@]}" test --workspace $CARGO_FLAGS

echo "==> 4/5 界面布局自检（无显示器）"
"${CARGO[@]}" build -p aevum-studio $CARGO_FLAGS
exe="$ROOT/target/debug/aevum.exe"
[ -f "$exe" ] || exe="$ROOT/target/debug/aevum-studio"
"$exe" --selftest

echo "==> 门槛全绿"
