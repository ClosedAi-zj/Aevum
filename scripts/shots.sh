#!/usr/bin/env bash
# 重新生成 shots/rust-ui-*.png：每张都由 lumen-studio 自己截，跑完就是当前代码的样子。
# 用法：bash scripts/shots.sh [--debug]（缺省用 release 包里的 exe）
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [ "${1:-}" = "--debug" ]; then
    EXE="target/debug/lumen.exe"
else
    EXE="dist/lumen/lumen.exe"
fi
[ -f "$EXE" ] || EXE="${EXE%.exe}"
if [ ! -f "$EXE" ]; then
    echo "找不到 $EXE，先跑 bash scripts/package.sh 或者加 --debug" >&2
    exit 1
fi

empty_dir="$(mktemp -d)"
trap 'rmdir "$empty_dir" 2>/dev/null || true' EXIT

shot() {
    local name="$1"
    shift
    rm -f "shots/$name"
    "$EXE" --shot "shots/$name" "$@" >/dev/null
    local size
    size="$(wc -c <"shots/$name")"
    # 空白页只会有一层底色，小于 60KB 基本就是没渲染出东西
    if [ "$size" -lt 60000 ]; then
        echo "$name 只有 $size 字节，疑似空白页" >&2
        exit 1
    fi
    echo "  $name  $size"
}

echo "==> 外壳与六模块"
shot rust-ui.png --module 显影 --demo-edit
shot rust-ui-empty.png --open "$empty_dir"
shot rust-ui-序列.png --module 序列
shot rust-ui-序列-体检.png --module 序列 --shot-qa --shot-group seq-qa
shot rust-ui-去闪.png --module 去闪
shot rust-ui-去闪-分通道.png --module 去闪 --shot-channels
shot rust-ui-显影.png --module 显影
shot rust-ui-构图.png --module 构图
shot rust-ui-构图-裁剪.png --module 构图 --shot-crop
shot rust-ui-构图-拉直.png --module 构图 --shot-rotate
shot rust-ui-成片.png --module 成片
shot rust-ui-成片-队列.png --module 成片 --shot-queue
shot rust-ui-成片-输出.png --module 成片 --shot-output --shot-group render-out
shot rust-ui-成片-水印.png --module 成片 --shot-watermark --shot-group render-watermark
shot rust-ui-成片-变速.png --module 成片 --shot-retime --shot-group render-retime
shot rust-ui-成片-运镜.png --module 成片 --shot-motion --shot-group render-motion
shot rust-ui-成片-运镜-画布.png --module 成片 --shot-motion
shot rust-ui-声音.png --module 声音

echo "==> 显影面板分组（U1 新参数）"
shot rust-ui-显影-基本.png --module 显影 --shot-group basic
shot rust-ui-显影-色彩.png --module 显影 --shot-group color
shot rust-ui-显影-曲线.png --module 显影 --shot-group curve
shot rust-ui-显影-色阶.png --module 显影 --shot-group levels
shot rust-ui-显影-hsl.png --module 显影 --shot-group hsl
shot rust-ui-显影-黑白.png --module 显影 --shot-group mono
shot rust-ui-显影-分级.png --module 显影 --shot-group grade
shot rust-ui-显影-细节.png --module 显影 --shot-group detail
shot rust-ui-显影-镜头.png --module 显影 --shot-group lens
shot rust-ui-显影-配置.png --module 显影 --shot-group profiles
shot rust-ui-显影-HDR.png --module 显影 --shot-group hdr
shot rust-ui-显影-全景.png --module 显影 --shot-group pano
shot rust-ui-显影-XMP.png --module 显影 --shot-end

echo "==> 蒙版与右侧面板"
shot rust-ui-显影-蒙版.png --module 显影 --demo-edit --shot-group masks
shot rust-ui-显影-蒙版-形状.png --module 显影 --demo-edit --shot-group mask-params
shot rust-ui-显影-蒙版-画笔.png --module 显影 --shot-brush --shot-group masks
shot rust-ui-信息-exif.png --shot-dock 信息
shot rust-ui-AI.png --shot-dock AI
shot rust-ui-历史.png --shot-dock 历史
shot rust-ui-关键帧.png --shot-dock 关键帧
shot rust-ui-图库.png --shot-library --shot-dock 图库
shot rust-ui-图库-信息.png --shot-library --shot-libmeta --shot-dock 图库
shot rust-ui-图库-堆叠.png --shot-library --shot-libstack --shot-dock 图库
shot rust-ui-图库-集合.png --shot-library --shot-collection --shot-dock 图库

echo "==> 全部生成，共 $(ls shots/rust-ui-*.png | wc -l) 张"
