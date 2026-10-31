#!/usr/bin/env bash
# 打 Windows 绿色包：先过 acceptance/check.sh 的门槛，再 release exe + 随包 ffmpeg，
# 然后自检、真跑一次导出，最后打出 zip 与 SHA256SUMS 并解压回验。
#
# 用法（Git Bash）：
#   scripts/package.sh                  # ffmpeg 探测：AEVUM_FFMPEG_SRC → 包内
#                                       # tools/ffmpeg.exe（上次打包留下的）→ PATH
#   AEVUM_FFMPEG_SRC=D:/tools/ffmpeg.exe scripts/package.sh
#   SKIP_CHECK=1 scripts/package.sh     # 只跳过门槛，其余步骤照跑
#
# 产物：dist/aevum/{aevum.exe, tools/ffmpeg.exe, tools/README-ffmpeg.txt,
#                      README.md, THIRD-PARTY-NOTICES.txt,
#                      licenses/, third-party-src/}
#       dist/Aevum-Studio-v<版本>-windows-x64.zip   # 顶层目录 + LICENSE + CHANGELOG.md
#       dist/SHA256SUMS                             # zip 的 sha256，sha256sum -c 可验
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
    bash "$ROOT/acceptance/check.sh"
fi

echo "==> 编译 release"
"${CARGO[@]}" build -p aevum-studio --release --offline

ff_src="${AEVUM_FFMPEG_SRC:-}"
# 第二优先级：上次打包留在 dist/aevum/tools/ 里的 ffmpeg。首次打包时该文件
# 不存在，-f 判断自然跳过，不会报错。曾经这里的第二优先级是某个 Python venv
# 的 imageio-ffmpeg 死路径（仓库改名前就失效了），已删——包内复用不依赖任何
# 机器上的第三方目录，探测链才是可复现的。
if [ -z "$ff_src" ] && [ -f "$DIST/tools/ffmpeg.exe" ]; then
    ff_src="$DIST/tools/ffmpeg.exe"
fi
if [ -z "$ff_src" ]; then
    ff_src="$(command -v ffmpeg || true)"
fi
if [ -z "$ff_src" ] || [ ! -f "$ff_src" ]; then
    # 别只报「没找到」——下一个人不知道下一步该干什么。
    cat >&2 <<EOF
找不到 ffmpeg，打包中止。

  默认探测顺序：AEVUM_FFMPEG_SRC → dist/aevum/tools/ffmpeg.exe（上次打包留下的） → PATH
  三条都没命中。（首次打包没有第二条，属正常，不算环境故障。）

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
# 探测链第二级命中的可能正是目标本身（上次打包留在 dist/aevum/tools/ 的 ffmpeg）。
# 这种情况不能再拷一次：coreutils 的 cp 对同一文件会直接报
# "are the same file" 并以非零退出，整个打包中止。首次打包时目标不存在，-ef 为假，
# 照常复制。这个坑只有"第二次及以后"的打包才会踩到，所以第一次跑通不算验过。
if [ ! "$ff_src" -ef "$DIST/tools/ffmpeg.exe" ]; then
    cp -f "$ff_src" "$DIST/tools/ffmpeg.exe"
fi
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
# 输出留着给后面的 zip 解压回验比对形状数：解压产物必须和构建树一模一样。
selftest_build="$("$DIST/aevum.exe" --selftest)"
echo "$selftest_build"

echo "==> 真实导出（只用随包 ffmpeg，清空 PATH 里的 ffmpeg 影响）"
# 显式指到 /tmp：Git Bash 的 mktemp 在某些环境（TMP 指到 Windows 临时目录）会
# 返回带盘符的路径，下游的 rm/工具链对盘符路径行为不一；/tmp 在 Git Bash 里
# 恒存在且始终是 POSIX 形式。本脚本只承诺在 Git Bash 里跑。
work="$(TMPDIR=/tmp mktemp -d)"
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

# ---- zip + SHA256SUMS（1.0.0 验收清单 B4）----
# zip 不再人工压：顶层目录、LICENSE、CHANGELOG.md、校验和都由本脚本保证。
# 版本号从唯一真源 Cargo.toml 读，不引第三方工具。
echo "==> 打 zip（顶层目录 Aevum-Studio-v<版本>/）"
ver="$(awk -F'"' '/^version /{print $2; exit}' Cargo.toml)"
zip_name="Aevum-Studio-v${ver}-windows-x64.zip"
zip_out="$ROOT/dist/$zip_name"

stage="$work/zipstage/Aevum-Studio-v$ver"
mkdir -p "$stage"
cp -a "$DIST/." "$stage/"
# 项目自身的 LICENSE 与变更记录随包分发——之前人工压的 zip 一直缺这两个。
cp -f "$ROOT/LICENSE" "$ROOT/CHANGELOG.md" "$stage/"

# 压缩用 Windows 内置 bsdtar（System32\tar.exe，libarchive），不用 PowerShell
# 的 Compress-Archive：PS 5.1 会把 zip 内路径分隔符写成反斜杠，跨平台解压工具
# 会把整条路径当成一个文件名；bsdtar 写正斜杠。Git Bash 自带的 GNU tar 不认
# zip 格式，所以这里必须显式走 System32 的那份。bsdtar 对非 ASCII 文件名会
# 按本地代码页写、可能乱码——包内容树全部是 ASCII 文件名，不受影响；
# 若日后要往包里放非 ASCII 文件名，先实测再改方案。
( cd "$work/zipstage" && /c/Windows/System32/tar.exe -a -c -f "$(cygpath -m "$zip_out")" "Aevum-Studio-v$ver" )

# 校验和只含 zip 一行：SHA256SUMS 自身没法校验自己（自指）。
zip_sha="$(sha256sum "$zip_out" | awk '{print $1}')"
printf '%s  %s\n' "$zip_sha" "$zip_name" > "$ROOT/dist/SHA256SUMS"

echo "==> 解压回验（zip 里跑 --selftest，形状数必须与构建树一致）"
# 这里用 rm -rf 是安全的：删的是本脚本自己 mktemp -d 出来的临时目录
# （$work 与 $zipcheck），里面的每一样东西都是本次运行创建的，
# 不含任何用户数据；trap 保证脚本无论成败退出前都清干净。
zipcheck="$(TMPDIR=/tmp mktemp -d)"
trap 'rm -rf "$work" "$zipcheck"' EXIT
unzip -q "$zip_out" -d "$zipcheck"
selftest_zip="$("$zipcheck/Aevum-Studio-v$ver/aevum.exe" --selftest)"
if [ "$selftest_zip" != "$selftest_build" ]; then
    echo "解压产物的自检结果与构建树不一致：" >&2
    echo "  构建树：$selftest_build" >&2
    echo "  解压后：$selftest_zip" >&2
    exit 1
fi

zip_bytes="$(wc -c < "$zip_out" | tr -d '[:space:]')"
echo "==> 完成：$DIST（$size）"
ls -la "$DIST" "$DIST/tools"
echo "==> zip：$zip_out"
echo "    大小：$zip_bytes 字节"
echo "    sha256：$zip_sha"
echo "    校验和：$ROOT/dist/SHA256SUMS（sha256sum -c dist/SHA256SUMS 可验）"
echo "    解压自检：与构建树一致 ✓"
