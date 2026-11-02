#!/usr/bin/env bash
# 重新生成 README 引用的五张截图（assets/shots/{develop,sequence,deflicker,compose,render}.png）。
#
# 与 scripts/shots.sh 的区别：那边是开发期的全量 UI 快照（几十张、演示序列），
# 这边只管 README 引用的五张，而且**必须用真实样张**（samples/cameras/ 的 PANA 四张），
# 不用演示夹具——README 是给别人看的，图里得是真照片。
#
# 用法：
#   bash scripts/shots-readme.sh            # 用 target/release/aevum.exe（先 cargo build --release）
#   bash scripts/shots-readme.sh --debug    # 用 target/debug/aevum.exe
#   bash scripts/shots-readme.sh /path/exe  # 显式指定
#
# 每张的守卫照 scripts/shots.sh 的 60KB 线；尺寸与断链的判据在 Rust 侧
# （crates/aevum-ui/tests/suite/readme_shots.rs，D-C1/D-C2）。
#
# 素材约束（D-C2）：**只读** samples/cameras/ 的四张 PANA；staging 目录开在
# target/readme-shots/（已被 .gitignore 忽略），仓库里不新增任何样张文件——
# 跑完 `git status` 必须还是干净的。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

case "${1:-}" in
    --debug) EXE="target/debug/aevum.exe" ;;
    "") EXE="target/release/aevum.exe" ;;
    *) EXE="$1" ;;
esac
[ -f "$EXE" ] || EXE="${EXE%.exe}"
if [ ! -f "$EXE" ]; then
    echo "找不到 $EXE：先 cargo build --release，或把 exe 路径作为第一个参数传入" >&2
    exit 1
fi

# staging：把四张真实样张拷进 target/readme-shots/ 当一个序列打开。
#
# 为什么不直接 --open samples/cameras：那个目录里还有 NEF/CR3/HEIC/ARW 等别家机型的
# 文件，混进来既不是「焦点包围」也不是 README 想展示的样子；而且截图进程会写缓存，
# 往 samples/ 里落文件就违反 D-C2 的「仓库不新增样张」。开在 target/ 下有两个好处：
# gitignore 已经盖住它（git status 保持干净），而且界面信息面板显示的序列根路径
# 在每次重截时**完全一致**，README 的图不会每次换一个临时目录名。
#
# Why not --open samples/cameras directly: that directory carries other cameras' files
# (NEF/CR3/HEIC/ARW), which is neither a focus bracket nor what the README shows; and the
# shot process writes caches, which would violate D-C2's "no new sample files in the repo".
# Staging under target/ keeps git status clean (it is ignored) and makes the sequence root
# shown in the info panel **identical across runs**, so the README shots do not change
# their temp directory name every time.
STAGING="$ROOT/target/readme-shots"
if command -v python >/dev/null 2>&1; then
    # 这台机器的 bash rm 会把文件搬进回收站（见 tools/hard-rm.ps1 的注释），删目录走 python。
    #
    # bash `rm` on this machine routes deletions into the Recycle Bin (see the note in
    # tools/hard-rm.ps1), so directory removal goes through python.
    python -c "import shutil,sys; shutil.rmtree(sys.argv[1], True)" "$STAGING"
fi
mkdir -p "$STAGING"

for f in PANA0001.RW2 PANA0450.RW2 PANA1631.RW2 PANA0198.JPG; do
    src="samples/cameras/$f"
    [ -f "$src" ] || { echo "缺样张 $src" >&2; exit 1; }
    cp "$src" "$STAGING/$f"
done

mkdir -p assets/shots

# name 与 README.md / README.en.md 的引用逐字一致，改这里必须同步改 README 与文档
# （readme_shots.rs 的断言会红）。
#
# Names must match README.md / README.en.md verbatim; changing them means changing the
# README and the docs too (the assertions in readme_shots.rs turn red otherwise).
shot() {
    local name="$1"
    shift
    local out="assets/shots/$name.png"
    rm -f "$out"
    "$EXE" --open "$STAGING" --shot "$out" "$@" >/dev/null
    local size
    size="$(wc -c <"$out")"
    # 空白页只会有一层底色，小于 60KB 基本就是没渲染出东西（照 shots.sh 的线）。
    #
    # A blank page is one flat fill; under 60 KB means nothing got rendered (same line as shots.sh).
    if [ "$size" -lt 60000 ]; then
        echo "$name 只有 $size 字节，疑似空白页" >&2
        exit 1
    fi
    echo "  $name.png  $size"
}

echo "==> README 五张（真实样张 staging：$STAGING）"
shot develop   --module develop --shot-group focus
shot sequence  --module sequence
shot deflicker --module deflicker --shot-channels
shot compose   --module frame
shot render    --module render

echo "==> 五张就绪，判据（尺寸/断链/记录一致）跑 cargo test -p aevum-ui --test suite readme_shots"
