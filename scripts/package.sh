#!/usr/bin/env bash
# 打 Windows 绿色包：先过 scripts/check.sh 的门槛，再 release exe + 随包 ffmpeg，
# 然后自检并真跑一次导出。
#
# 用法（Git Bash）：
#   scripts/package.sh                  # 默认从 Python venv 的 imageio-ffmpeg 取 ffmpeg
#   AEVUM_FFMPEG_SRC=D:/tools/ffmpeg.exe scripts/package.sh
#   SKIP_CHECK=1 scripts/package.sh     # 只跳过门槛，其余步骤照跑
#
# 产物：dist/aevum/{aevum.exe, tools/ffmpeg.exe, tools/README-ffmpeg.txt,
#                      README.md, THIRD-PARTY-NOTICES.txt,
#                      licenses/, third-party-src/}
#
# licenses/ 与 third-party-src/ 不是可选的装饰：
#   - LGPL-2.1 / OFL / UFL / MIT 都要求"随分发附带许可文本"，少了就是分发不合规；
#   - rawler 以 LGPL-2.1 静态链接进 aevum.exe，必须一并提供其对应源码。
# 详见 THIRD-PARTY-NOTICES.txt 第 2 节。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="$ROOT/dist/aevum"
cd "$ROOT"

# 本机 gitconfig 里有死代理，cargo 必须临时清掉代理变量。
CARGO=(env -u http_proxy -u https_proxy -u HTTP_PROXY -u HTTPS_PROXY \
    -u all_proxy -u ALL_PROXY CARGO_HTTP_PROXY= cargo)

if [ "${SKIP_CHECK:-}" = "1" ]; then
    echo "==> 跳过门槛（SKIP_CHECK=1）"
else
    echo "==> 门槛：格式 / clippy / 全量测试 / 布局自检"
    bash "$ROOT/scripts/check.sh"
fi

echo "==> 编译 release"
"${CARGO[@]}" build -p aevum-studio --release --offline

ff_src="${AEVUM_FFMPEG_SRC:-}"
if [ -z "$ff_src" ]; then
    default_ff="$ROOT/../pythonProject4/.venv/Lib/site-packages/imageio_ffmpeg/binaries/ffmpeg-win-x86_64-v7.1.exe"
    if [ -f "$default_ff" ]; then
        ff_src="$default_ff"
    fi
fi
if [ -z "$ff_src" ]; then
    ff_src="$(command -v ffmpeg || true)"
fi
if [ -z "$ff_src" ] || [ ! -f "$ff_src" ]; then
    # 别只报「没找到」——下一个人不知道下一步该干什么。
    # 注意：上面那条 $ROOT/../pythonProject4/.venv/... 的默认路径在多数机器上
    # 是不存在的（改名之前就失效了），所以走到这里很正常，不是环境坏了。
    cat >&2 <<EOF
找不到 ffmpeg，打包中止。

  默认探测顺序：AEVUM_FFMPEG_SRC → $ROOT/../pythonProject4/.venv/.../ffmpeg-win-x86_64-v7.1.exe → PATH
  三条都没命中。（默认的 venv 路径在多数机器上本来就不存在，这不是环境故障。）

下一步，显式指一个 ffmpeg 可执行文件：

  AEVUM_FFMPEG_SRC='D:/tools/ffmpeg.exe' scripts/package.sh

或者是 Git Bash 里先把它放进 PATH：

  export PATH="/d/tools/ffmpeg/bin:\$PATH" && scripts/package.sh

注意：本脚本只认新名 AEVUM_FFMPEG_SRC。旧名 LUMEN_FFMPEG_SRC 已不再读取，
     设了也不会生效（0.4 及更早的文档教的就是那个名字，容易踩空）。
EOF
    exit 1
fi

echo "==> 组装 $DIST"
mkdir -p "$DIST/tools"
cp -f "$ROOT/target/release/aevum.exe" "$DIST/"
cp -f "$ff_src" "$DIST/tools/ffmpeg.exe"
cp -f "$ROOT/README.md" "$DIST/"
# 随包 ffmpeg 是 GPLv3 构建，分发时必须带许可与源码获取方式（见该文件的说明）。
cp -f "$ROOT/THIRD-PARTY-NOTICES.txt" "$DIST/"

# ---- 许可文本：LGPL-2.1 / OFL-1.1 / UFL-1.0 / MIT / ISC 都要求随分发附带 ----
rm -rf "$DIST/licenses"
cp -rf "$ROOT/licenses" "$DIST/licenses"
# 项目自选字体与图标的许可文本散在 assets/ 下，统一归到 licenses/，让随包只有一处要找。
cp -f "$ROOT/assets/fonts/OFL-Inter.txt"        "$DIST/licenses/OFL-1.1-Inter.txt"
cp -f "$ROOT/assets/fonts/OFL-NotoSansSC.txt"   "$DIST/licenses/OFL-1.1-Noto-Sans-SC.txt"
cp -f "$ROOT/assets/icons/LICENSE-Lucide.txt"   "$DIST/licenses/ISC-Lucide.txt"

# ---- rawler 源码：LGPL-2.1 §6 要求提供对应源码，直接附 Cargo 源码包（约 730 KB）----
# 找不到时不阻断打包（换个容器/CI 可能没有 registry 缓存），但必须喊出来，
# 并把下载地址打在随包说明里——静默少一个文件才是最坏的结果。
rm -rf "$DIST/third-party-src"
mkdir -p "$DIST/third-party-src"
rawler_src="${AEVUM_RAWLER_SRC:-}"
if [ -z "$rawler_src" ]; then
    rawler_src="$(ls "${CARGO_HOME:-$HOME/.cargo}"/registry/cache/*/rawler-*.crate 2>/dev/null | sort | tail -n 1 || true)"
fi
if [ -n "$rawler_src" ] && [ -f "$rawler_src" ]; then
    cp -f "$rawler_src" "$DIST/third-party-src/"
    echo "    随包源码：$(basename "$rawler_src")  $(sha256sum "$rawler_src" | cut -c1-16)…"
else
    echo "    ⚠ 没找到 rawler 的 .crate 源码包，third-party-src/ 为空。" >&2
    echo "      LGPL-2.1 要求随二进制提供对应源码，请用 AEVUM_RAWLER_SRC=<路径> 重跑，" >&2
    echo "      或从 https://crates.io/api/v1/crates/rawler/0.7.2/download 下载后放入。" >&2
fi

ff_version="$("$DIST/tools/ffmpeg.exe" -hide_banner -version | head -n 1)"
cat > "$DIST/tools/README-ffmpeg.txt" <<EOF
随包 ffmpeg 供导出成片与探测音频使用，不参与界面绘制。

版本：$ff_version
许可：GPLv3（该构建启用 --enable-gpl --enable-version3）
源码：https://ffmpeg.org/download.html
EOF

echo "==> 自检（无显示器）"
"$DIST/aevum.exe" --selftest

echo "==> 真实导出（只用随包 ffmpeg，清空 PATH 里的 ffmpeg 影响）"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
work_win="$(cygpath -m "$work")"

"$DIST/tools/ffmpeg.exe" -y -hide_banner -loglevel error \
    -f lavfi -i "color=c=0x404040:s=64x48" -frames:v 3 "$work/frame_%03d.png"

req_open="$(printf '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"aevum_open_sequence","arguments":{"root":"%s"}}}' "$work_win")"
req_export="$(printf '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"aevum_export_video","arguments":{"out_dir":"%s","video_out":"%s","width":64,"height":48,"fps":24,"codec":"h264","crf":23,"preset":"ultrafast","confirm":true}}}' \
    "$work_win/frames_out" "$work_win/pack-check.mp4")"

{
    printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"pack-check","version":"1"}}}'
    printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
    printf '%s\n' "$req_open"
    printf '%s\n' "$req_export"
} | env -u AEVUM_FFMPEG "$DIST/aevum.exe" --mcp > "$work/mcp.log"

grep -q '"id":2' "$work/mcp.log" || { echo "MCP 没有回应打开序列" >&2; cat "$work/mcp.log" >&2; exit 1; }
# 工具结果是嵌在 content[0].text 里的 JSON 字符串，引号带转义。
grep -qF '\"frames\":3' "$work/mcp.log" || { echo "打开序列结果不对" >&2; cat "$work/mcp.log" >&2; exit 1; }
grep -q '"id":3' "$work/mcp.log" || { echo "MCP 没有回应导出" >&2; cat "$work/mcp.log" >&2; exit 1; }
if grep -q '"isError":true' "$work/mcp.log"; then
    echo "导出失败：" >&2
    cat "$work/mcp.log" >&2
    exit 1
fi
[ -s "$work/pack-check.mp4" ] || { echo "没有生成成片" >&2; exit 1; }

size="$(du -sh "$DIST" | cut -f1)"
echo "==> 完成：$DIST（$size）"
ls -la "$DIST" "$DIST/tools"
