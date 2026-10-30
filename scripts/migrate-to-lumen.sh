#!/usr/bin/env bash
# 把改名前留下的旧名字搬成新名字：
#   1. 序列根目录里的 .lapse/ 护带目录 → .lumen/
#   2. %APPDATA%/lapse-studio/render_queue.json → %APPDATA%/lumen-studio/
#
# 程序本身在打开序列时会顺手搬第 1 条，所以只有一两个序列要用的话不必跑这里；
# 这脚本是给整盘素材一次性搬完准备的。缺省只报告不动手，加 --apply 才真改。
#
# 用法（Git Bash）：
#   scripts/migrate-to-lumen.sh 'F:/2025'            # 先看看会动哪些
#   scripts/migrate-to-lumen.sh 'F:/2025' --apply    # 真搬
set -euo pipefail

root=""
apply=0
for arg in "$@"; do
    case "$arg" in
        --apply) apply=1 ;;
        -h | --help)
            sed -n '2,12p' "${BASH_SOURCE[0]}"
            exit 0
            ;;
        *) root="$arg" ;;
    esac
done
if [ -z "$root" ] || [ ! -d "$root" ]; then
    echo "用法：scripts/migrate-to-lumen.sh <要扫描的目录> [--apply]" >&2
    exit 2
fi

move() {
    local from="$1" to="$2"
    if [ "$apply" = "1" ]; then
        mkdir -p "$(dirname "$to")"
        mv -n "$from" "$to"
        echo "  搬：$from → $to"
    else
        echo "  会搬：$from → $to"
    fi
}

echo "==> 扫描 $root 下的 .lapse 护带目录"
moved=0
skipped=0
# 只认目录：Python 版在序列根上留过 .lapse_ratings.json 这类同名前缀的文件，那不是护带目录。
while IFS= read -r -d '' old; do
    parent="$(dirname "$old")"
    new="$parent/.lumen"
    if [ -e "$new" ]; then
        echo "  跳过：$old（旁边已经有 .lumen，不猜哪边是对的）"
        skipped=$((skipped + 1))
        continue
    fi
    move "$old" "$new"
    moved=$((moved + 1))
done < <(find "$root" -depth -type d -name '.lapse' -print0)
echo "  共 $moved 个，跳过 $skipped 个"

echo "==> 漫游配置里的渲染队列"
appdata="${APPDATA:-}"
if [ -z "$appdata" ]; then
    appdata="$(cygpath -u "$(cmd.exe /c 'echo %APPDATA%' 2>/dev/null | tr -d '\r')" 2>/dev/null || true)"
fi
if [ -n "$appdata" ] && [ -d "$appdata/lapse-studio" ]; then
    if [ -e "$appdata/lumen-studio/render_queue.json" ]; then
        echo "  跳过：已经有 $appdata/lumen-studio/render_queue.json"
    else
        move "$appdata/lapse-studio/render_queue.json" "$appdata/lumen-studio/render_queue.json" 2>/dev/null ||
            echo "  没有 $appdata/lapse-studio/render_queue.json，只有目录本身"
    fi
else
    echo "  没找到 $appdata/lapse-studio，跳过"
fi

if [ "$apply" = "0" ]; then
    echo "==> 这是预演，什么都没动。确认无误后加 --apply。"
fi
