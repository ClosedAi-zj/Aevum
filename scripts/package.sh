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
#                      README.md, THIRD-PARTY-NOTICES.txt}
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
