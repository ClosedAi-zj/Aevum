#!/usr/bin/env bash
# 把改名前留下的旧名字一次性搬成新名字（三级链的上两级，最老的 lapse 也在内）：
#
#   1. 序列根目录里的护带目录：.lumen/ → .aevum/，.lapse/ → .aevum/
#      同一个序列根里两个旧名字都在时，只搬 .lumen（更新的那个），.lapse 原样留着，
#      因为分不清哪份才是用户现在在用的，猜错了就是覆盖数据。
#   2. 漫游配置目录：%APPDATA%/lumen-studio → %APPDATA%/aevum-studio
#      （没有 lumen-studio 时再看 lapse-studio）。目标是整目录级迁移，
#      settings.json / galleries.json / plugins/ 一个都不落；
#      目标已存在时逐项补缺，**目标已有的以目标为准**，且旧目录一律不删。
#
# 程序本身在打开序列 / 首次读配置时会自动做这两件事，所以只有一两个序列要用的
# 话不必跑这里；这脚本是给「整盘素材一次性搬完、想先看看会动什么」准备的。
# 缺省只报告不动手，加 --apply 才真改。
#
# 用法（Git Bash）：
#   scripts/migrate-to-aevum.sh 'F:/2025'            # 先看看会动哪些
#   scripts/migrate-to-aevum.sh 'F:/2025' --apply    # 真搬
set -euo pipefail

root=""
apply=0
for arg in "$@"; do
    case "$arg" in
        --apply) apply=1 ;;
        -h | --help)
            sed -n '2,24p' "${BASH_SOURCE[0]}"
            exit 0
            ;;
        *) root="$arg" ;;
    esac
done
if [ -z "$root" ] || [ ! -d "$root" ]; then
    echo "用法：scripts/migrate-to-aevum.sh <要扫描的目录> [--apply]" >&2
    exit 2
fi

# 单条搬运。mv -n 对目录也成立（目标存在就拒搬），所以不会覆盖。
move() {
    local from="$1" to="$2"
    if [ "$apply" = "1" ]; then
        mkdir -p "$(dirname "$to")"
        if mv -n "$from" "$to" 2>/dev/null; then
            echo "  搬：$from → $to"
        else
            echo "  失败（可能被占用）：$from" >&2
            return 1
        fi
    else
        echo "  会搬：$from → $to"
    fi
}

# 逐项补缺：目标缺什么补什么，目标已有的跳过（以目标为准）。递归，不删源。
merge_into() {
    local from="$1" to="$2"
    if [ "$apply" = "1" ]; then
        mkdir -p "$to"
        if ! cp -an "$from"/. "$to"/ 2>/dev/null; then
            echo "  补缺失败（可能被占用）：$from" >&2
            return 1
        fi
        echo "  补齐：$from/* → $to/（同名以目标为准）"
    else
        echo "  会补齐：$from/* → $to/（同名以目标为准）"
    fi
}

echo "==> 扫描 $root 下的旧护带目录（按 .lumen 优先、.lapse 兜底的顺序）"
moved=0
skipped=0
# 预演模式不真搬，所以「目标已存在」必须自己记账：`c/.lumen` 被判要搬去
# `c/.aevum` 之后，同一轮里的 `c/.lapse` 就得看见那个还没落地的目标，
# 否则预演会报出两条互相冲突的搬运，真跑时第二条又会被 `mv -n` 静默吞掉。
declare -A taken=()
for old_name in .lumen .lapse; do
    # 只认目录：Python 版在序列根上留过 .lapse_ratings.json 这类同名前缀的文件，
    # 那不是护带目录，碰了就是把用户的评分数据改名。
    while IFS= read -r -d '' old; do
        parent="$(dirname "$old")"
        new="$parent/.aevum"
        if [ -e "$new" ] || [ -n "${taken[$new]:-}" ]; then
            echo "  跳过：$old（同一位置已有更近的护带目录，不猜哪边是对的）"
            skipped=$((skipped + 1))
            continue
        fi
        taken["$new"]=1
        if move "$old" "$new"; then
            moved=$((moved + 1))
        fi
    done < <(find "$root" -type d -name "$old_name" -print0 2>/dev/null || true)
done
echo "  共 $moved 个，跳过 $skipped 个"

echo "==> 漫游配置目录（整目录级，含 settings/galleries/plugins）"
appdata="${APPDATA:-}"
if [ -z "$appdata" ]; then
    appdata="$(cygpath -u "$(cmd.exe /c 'echo %APPDATA%' 2>/dev/null | tr -d '\r')" 2>/dev/null || true)"
fi
fresh="$appdata/aevum-studio"
# 从新到旧找第一个存在的旧目录，只处理它：两个都在说明中间跨了两个版本，
# 把更老那份也并进来等于替用户猜数据。
old_cfg=""
for name in lumen-studio lapse-studio; do
    if [ -n "$appdata" ] && [ -d "$appdata/$name" ]; then
        old_cfg="$appdata/$name"
        break
    fi
done
if [ -z "$old_cfg" ]; then
    echo "  没找到 $appdata/lumen-studio 或 lapse-studio，跳过"
elif [ ! -e "$fresh" ]; then
    move "$old_cfg" "$fresh"
else
    merge_into "$old_cfg" "$fresh"
    echo "  旧目录保留在 $old_cfg（不删：用户随时能回去翻）"
fi

if [ "$apply" = "0" ]; then
    echo "==> 这是预演，什么都没动。确认无误后加 --apply。"
fi
