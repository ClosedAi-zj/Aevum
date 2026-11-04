#!/usr/bin/env bash
# 提交门槛：版本一致性 → 格式 → clippy（警告即失败）→ 全量测试 → 界面布局自检 → 帧时预算（release）。
#
# 用法（Git Bash）：
#   acceptance/check.sh                 # 本机默认离线，用已缓存的依赖
#   CARGO_FLAGS= acceptance/check.sh    # CI 上允许联网解析依赖
#
# 六步任何一步红就停，别把脏改动带进 scripts/package.sh。
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

# 收尾：清本盘回收站（用户 2026-11-05 明确要求「每次构建后清理回收站」）。
# 这台机器上的删除被拦进 $Recycle.Bin，不清等于没删 —— 2026-11-05 实测 bin 涨到
# 402,058 项 / 114.34 GB、E 盘只剩 5 GB，链接器随即 LNK1180/LNK1201，门槛跑到一半
# 就会卡死。挂在 EXIT 上，关卡红也照清：拦下来的正是「跑不下去」的那一次。
# 清 bin 失败不影响退出码（收尾不是判据，见 tools/purge-bin.sh）。
#
# Housekeeping: purge this drive's Recycle Bin (asked for on 2026-11-05). Deletion is
# intercepted into $Recycle.Bin on this machine, so without a purge nothing is
# reclaimed -- measured on 2026-11-05: 402,058 items / 114.34 GB with 5 GB free,
# followed by linker errors LNK1180/LNK1201 and a gate that hangs halfway. It hangs on
# EXIT so a red gate is cleaned too -- a red gate is exactly the run that stopped short.
# A failed purge does not change the exit code (see tools/purge-bin.sh).
BIN_DRIVE="${AEVUM_BIN_DRIVE:-$(cygpath -m "$ROOT" | cut -d/ -f1)}"
trap 'bash "$ROOT/tools/purge-bin.sh" "$BIN_DRIVE" || true' EXIT

# 第 0 关：版本号一致性（1.0.0 验收清单 B5，beta.2 当天就漂移过一次 README）
# + 文档双语一致性（中英分册之后：缺孪生、缺切换链接、死链都是静默失效）
# + 代码注释双语（模块级 //! 与 pub 项的 /// 必须有英文块）
# + 变异残留扫描（见下）
# 只读文件、不碰 cargo，所以 CARGO_FLAGS（CI 联网 / 本机 --offline）对它无影响。
echo "==> 0/6 版本与文档一致性（Cargo.toml 真源 → lock / README / CHANGELOG；中英孪生；注释双语；变异残留）"
python "$ROOT/acceptance/check_version.py"
python "$ROOT/acceptance/check_docs.py"
python "$ROOT/tools/doc_lint.py"

# 变异残留扫描。**这条是为了堵一个真实事故**：2026-11-03 一个 `if false &&` 被提交进
# HEAD，预览草图的半尺寸快路在出货代码里死掉，而六关照样全绿——因为时延判据只量墙钟，
# 关掉快路后仍在预算内。事后判据改成了结构证据，但「变异被顺手提交」这件事本身还得有
# 一道零成本的闸。
#
# 判定：行首到命中点之间出现 `/` 的行按注释放过（`/// 提到 todo!()` 是合法文档）；
# 真实代码里「除号后面紧跟这几个词」的写法不存在，所以不会误放过。
#
# Mutation residue. This closes a real incident: on 2026-11-03 an `if false &&` was
# committed, the preview sketch's half-size fast path was dead in shipped code, and the six
# gates stayed green because the latency criterion only measures wall clock and the disabled
# path still fit the budget. The criterion is now structural, but "a mutation got committed"
# still deserves a zero-cost gate. Lines with a `/` before the match are treated as comments.
residue="$(git grep -nE '^[^/]*[^/](if \(?false\)?[ &{]|dbg!\(|todo!\(|unimplemented!\()' -- '*.rs' || true)"
if [ -n "$residue" ]; then
    # 别在双引号里写反引号：`` `if false` `` 会被 shell 当命令执行（这条自己踩过一次）。
    echo '✗ 代码里还有变异/调试残留（if false / dbg! / todo!() / unimplemented!()）：'
    echo "$residue"
    echo "  这些要么是变异验证没还原，要么是占位实现。还原或补实现后再提交。"
    exit 1
fi

echo "==> 1/6 格式"
"${CARGO[@]}" fmt --all -- --check

echo "==> 2/6 clippy（-D warnings）"
"${CARGO[@]}" clippy --workspace --all-targets $CARGO_FLAGS -- -D warnings

echo "==> 3/6 全量测试（含与 Python 基准的金标比对）"
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

echo "==> 4/6 界面布局自检（无显示器）"
"${CARGO[@]}" build -p aevum-studio $CARGO_FLAGS
exe="$ROOT/target/debug/aevum.exe"
[ -f "$exe" ] || exe="$ROOT/target/debug/aevum-studio"
"$exe" --selftest

# 第 5 关：帧时预算必须跑 **release**。
# preview_budget 的 300ms 是绝对预算，判据只在 release 下断言（dev 档 opt-level 1
# 会让「机器慢」冒充「退化」；dev 只打表）。前五关全是 dev，等于这条预算从来没被卡过。
# 代价：冷编译约 3.5 分钟，热跑 8 秒；不值得为省这点时间留一个不卡的闸门。
echo "==> 5/6 帧时预算（release：首屏 300ms / 面板 16ms 是绝对值，dev 档只打表不断言）"
"${CARGO[@]}" test -p aevum-ui --release --test suite preview_budget $CARGO_FLAGS
"${CARGO[@]}" test -p aevum-ui --release --test suite perf_budget $CARGO_FLAGS

echo "==> 门槛全绿"
