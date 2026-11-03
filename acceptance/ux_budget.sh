#!/usr/bin/env bash
# 人机体验三条硬预算的验收脚本（人工跑，不进六关门槛）。
#
# 为什么不进 `acceptance/check.sh`：它要起真窗口、还要 10 秒墙钟量空闲占用，
# CI 无头会假红。六关管的是「代码对不对」，这个脚本管的是「用户受不受得了」。
#
# 三条预算与来源：
#   - 启动到首帧 ≤ 1500ms：`--bench-startup` 的 CPU 墙钟（不含 GPU 呈现），5 次取中位。
#     这是**回归判据**：任何一次改动把它推过线都算退化。
#   - 空闲 CPU ≤ 1.0%：窗口起来后静置 10 秒，进程消耗的 CPU 时间占比（占一个核）。
#     超过 1% 意味着用户什么都不做也在烧电、风扇会转。
#   - 峰值工作集 ≤ 4.14GB：打开一张 24MP RAW 后进程的峰值内存。4.14GB 是改口径时
#     实测的峰值基线，超过它说明某处又整幅复制了一份 24MP 数据。
#
# 用法（Git Bash）：
#   acceptance/ux_budget.sh
#   acceptance/ux_budget.sh --debug          # 用 dev 档量（数字会差一点，仅排查用）
#   AEVUM_SAMPLE=<别的 RAW> acceptance/ux_budget.sh
#
# 输出末段是三条 ✓/✗，任何一条 ✗ 都让脚本以 1 退出。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PROFILE=release
CARGO_PROFILE_ARGS=(--release)
for arg in "$@"; do
    case "$arg" in
    --debug)
        PROFILE=debug
        CARGO_PROFILE_ARGS=()
        ;;
    *)
        echo "未知参数：$arg（只认 --debug）" >&2
        exit 2
        ;;
    esac
done

# 本机 gitconfig 里有死代理，cargo 必须临时清掉（与 check.sh 同一套）。
CARGO=(env -u http_proxy -u https_proxy -u HTTP_PROXY -u HTTPS_PROXY \
    -u all_proxy -u ALL_PROXY CARGO_HTTP_PROXY= CARGO_INCREMENTAL=0 cargo)
CARGO_FLAGS="${CARGO_FLAGS---offline}"

STARTUP_BUDGET_MS=1500
IDLE_BUDGET_PCT=1.0
PEAK_BUDGET_GB=4.14

SAMPLE="${AEVUM_SAMPLE:-$ROOT/samples/cameras/PANA0450.RW2}"
if [ ! -f "$SAMPLE" ]; then
    echo "⚠ 找不到样张 $SAMPLE：峰值内存与空闲占用会退化成「空窗口」，数字偏小。" >&2
    echo "  样张不入库，清单见 samples/cameras/README。设 AEVUM_SAMPLE 指向一张 24MP RAW。" >&2
    SAMPLE=""
fi

echo "==> 构建（$PROFILE）"
"${CARGO[@]}" build -p aevum-studio $CARGO_FLAGS "${CARGO_PROFILE_ARGS[@]}"

exe="$ROOT/target/$PROFILE/aevum.exe"
[ -f "$exe" ] || exe="$ROOT/target/$PROFILE/aevum-studio"
if [ ! -f "$exe" ]; then
    echo "✗ 找不到可执行文件：$ROOT/target/$PROFILE/aevum.exe" >&2
    exit 1
fi

echo "==> 启动到首帧（5 次冷进程取中位）"
startup_ms=()
for i in 1 2 3 4 5; do
    # 每次都是新进程：在进程内循环量第二次就不是冷启动了。
    out="$("$exe" --bench-startup 2>&1 | sed -n 's/.*启动到首帧 = \([0-9][0-9]*\)ms.*/\1/p' | head -1)"
    if [ -z "$out" ]; then
        echo "✗ 第 $i 次 --bench-startup 没打出数字，输出如下：" >&2
        "$exe" --bench-startup 2>&1 | tail -5 >&2 || true
        exit 1
    fi
    startup_ms+=("$out")
    printf '    第 %d 次：%sms\n' "$i" "$out"
done
median_ms="$(printf '%s\n' "${startup_ms[@]}" | sort -n | sed -n '3p')"
min_ms="$(printf '%s\n' "${startup_ms[@]}" | sort -n | sed -n '1p')"
max_ms="$(printf '%s\n' "${startup_ms[@]}" | sort -n | sed -n '5p')"
echo "    中位 ${median_ms}ms（${min_ms}…${max_ms}）"

echo "==> 空闲 CPU / 峰值内存（起真窗口，静置 10 秒）"
probe_args=(-NoProfile -ExecutionPolicy Bypass -File "$ROOT/tools/ux_probe.ps1" -Exe "$exe" -Seconds 10)
if [ -n "$SAMPLE" ]; then
    probe_args+=(-Sample "$SAMPLE")
fi
probe="$(powershell "${probe_args[@]}")"

# 探针只交 ASCII 键值（PS 5.1 往管道写中文是 OEM 代码页，bash 按 UTF-8 解会乱码），
# 中文措辞在这里拼。
idle_pct="$(printf '%s\n' "$probe" | sed -n 's/.*IDLE_CPU_PCT=\([0-9][0-9.]*\).*/\1/p' | head -1)"
peak_gb="$(printf '%s\n' "$probe" | sed -n 's/.*PEAK_WS_GB=\([0-9][0-9.]*\).*/\1/p' | head -1)"
interactive_ms="$(printf '%s\n' "$probe" | sed -n 's/.*STARTUP_INTERACTIVE_MS=\([0-9][0-9]*\).*/\1/p' | head -1)"
if [ -z "$idle_pct" ] || [ -z "$peak_gb" ]; then
    echo "✗ 探针输出解析不出来，原样输出如上：" >&2
    printf '%s\n' "$probe" >&2
    exit 1
fi
echo "    空闲 CPU ${idle_pct}%（10s，占一个核）/ 峰值工作集 ${peak_gb} GB / 启动到可交互 ${interactive_ms}ms"

# 比较用 awk：bash 只做整数比较，这三条都是小数。
cmp_ok() { awk -v a="$1" -v b="$2" 'BEGIN { exit (a <= b) ? 0 : 1 }'; }

fail=0
verdict() { # $1=标签 $2=实测 $3=预算 $4=单位
    if cmp_ok "$2" "$3"; then
        printf '  ✓ %s：%s%s ≤ %s%s\n' "$1" "$2" "$4" "$3" "$4"
    else
        printf '  ✗ %s：%s%s > %s%s  ← 超预算\n' "$1" "$2" "$4" "$3" "$4"
        fail=1
    fi
}

echo
echo "==> 结论"
verdict "启动到首帧（中位）" "$median_ms" "$STARTUP_BUDGET_MS" "ms"
verdict "空闲 CPU（10s）" "$idle_pct" "$IDLE_BUDGET_PCT" "%"
verdict "峰值工作集" "$peak_gb" "$PEAK_BUDGET_GB" "GB"
if [ -n "$interactive_ms" ]; then
    echo "  · 启动到可交互：${interactive_ms}ms（含窗口动画与 DWM 合成，只作参考，不作判据）"
fi
if [ -z "$SAMPLE" ]; then
    echo "  ⚠ 这次没带样张：空闲与峰值都是空窗口的数，别当 24MP 场景的结论。"
fi

if [ "$fail" -ne 0 ]; then
    exit 1
fi
echo "  三条全过。"
