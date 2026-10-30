# 版本变更记录

恒晷（Aevum Studio）的版本变更记录。每条只写**用户能感知到**的东西：行为变化、兼容策略、
已知问题、怎么回滚。内部实现细节、测试与金标数据不在本文件里。

> **beta 期不承诺兼容**：`0.x` 阶段工程文件格式、预设、命令行参数、MCP 工具集都可能变。
> 每次不兼容变更都会在本文件里单独标明，并尽量提供迁移路径或旧版回退办法。

---

## v0.5.0-beta.1 — 2026-10-31

**产品改名：Lumen Studio / 光时 → Aevum Studio / 恒晷。**
这是本版唯一的破坏性变更。**算法一行未改**：全部数值金标逐字节不变，界面自检的八项形状数与
0.4 完全一致。

### 改名

| 项 | 0.4 | 0.5 |
|---|---|---|
| 英文名 | Lumen Studio | **Aevum Studio** |
| 中文名 | 光时 | **恒晷** |
| 窗口标题 | 光时 · Lumen Studio | **恒晷 · Aevum Studio** |
| 可执行文件 | `lumen.exe` | **`aevum.exe`** |
| 绿色包目录 | `dist/lumen/` | **`dist/aevum/`** |
| 启动脚本 | `光时.bat` | **`恒晷.bat`** |
| crate 名 | `lumen-core` / `lumen-ui` / `lumen-mcp` | `aevum-core` / `aevum-ui` / `aevum-mcp` |
| 工程目录 | `<序列>/.lumen/` | **`<序列>/.aevum/`** |
| 配置目录 | `%APPDATA%/lumen-studio` | **`%APPDATA%/aevum-studio`** |
| 环境变量 | `LUMEN_*` | **`AEVUM_*`** |
| MCP 工具名 | `lumen_*` | **`aevum_*`** |

### 升级须知（三句话）

1. **旧工程的 `.lumen/` 会在你第一次打开那个序列时自动搬成 `.aevum/`。** 搬失败（只读盘、
   杀软正占用文件）时**继续在旧目录里工作**——不会丢数据，也不会变成只读。手动整盘搬迁见
   `scripts/migrate-to-aevum.sh`。
2. **环境变量 `LUMEN_*` 仍然可用**，但每个进程第一次用到时会在 stderr 打一行弃用提示。
   建议改成 `AEVUM_*`；两个都设时以 `AEVUM_` 为准。
3. **MCP 客户端升级后请重新拉取一次 `tools/list`。** 旧工具名 `lumen_*` 服务端仍然认
   （静默别名，返回结果逐字节相同），但**列表里只列 `aevum_*`**；有客户端会在调用前校验
   「这个名字必须出现在 list 里」，那种情况下旧名会被**客户端自己**拦掉，重拉即可恢复。

> **⚠️ 为什么旧工具名会被拦掉**
>
> 服务端其实接得住 `lumen_*`，挡住它的是**客户端自己**：有些客户端在发起 `tools/call`
> 之前会先校验「这个名字必须出现在我缓存的 `tools/list` 里」，而新版列表只列 `aevum_*`。
> 重新拉一次 `tools/list` 即恢复，不需要改任何配置。
>
> 另外 server 名由 `lumen-studio` 变成 `aevum-studio`，按 server 名做权限记忆的客户端会
> 重新询问一次授权——这是应有的行为，不是缺陷。

### 兼容性

1. **旧工程能直接开。** `.lumen/` 自动搬成 `.aevum/`，更早的 `.lapse/` 同样能搬。参数、评级、
   标记、关键词、关键帧、对齐结果、去闪曲线、取景框、运镜、水印设置全部原样恢复。
   两个旧目录同时存在时只用 `.lumen`（更新的那个），`.lapse` 原样留着不删。
   搬之前会先看 `.aevum/` 是不是**目录**：万一那里已经有个同名的普通**文件**，程序不覆盖它，
   而是继续用旧目录并在终端提示一句——搬目录时顺手把用户的文件抹掉，是最不该发生的那种事。
2. **旧环境变量仍然生效。** 12 个运行时变量（`LUMEN_LENSFUN` / `LUMEN_FFMPEG` / `LUMEN_LIBRARY` /
   `LUMEN_SETTINGS` / `LUMEN_PLUGINS` / `LUMEN_DCP` / `LUMEN_LCP` / `LUMEN_RENDER_QUEUE` /
   `LUMEN_FRAMES` / `LUMEN_DEV_CASES` / `LUMEN_REAL_RAW` / `LUMEN_ICC_DUMP`）全部支持旧名回退，
   新名优先，旧名首次命中打一行弃用提示。**唯一的例外是打包变量 `AEVUM_FFMPEG_SRC`**——
   它只给 `scripts/package.sh` 用，没有旧名回退，本版起不再读 `LUMEN_FFMPEG_SRC`。
   便携盘用法不变：设任一 override 即视为便携模式，完全不碰 `%APPDATA%`。
3. **旧配置目录首次启动自动并过来。** `%APPDATA%/lumen-studio`（或更早的 `lapse-studio`）搬成
   `%APPDATA%/aevum-studio`：新目录不存在时整目录改名（旧路径不再存在，但一个文件都没删）；
   新目录已存在时逐项补缺，同名以新目录为准，旧目录原样保留。迁移只做一次。

   > 上一代迁移脚本只搬过 `render_queue.json` 一个文件，导致从 Python 版升上来的用户丢过
   > `settings.json` / `galleries.json` / `plugins/`。本版改成整目录级迁移，并补上了 Rust 侧
   > 一直缺失的 `lapse-studio` 回退。当年已经丢掉的无法凭空找回，本版做的是「以后不再丢」。

4. **旧 MCP 工具名仍然可调。** 34 个 `lumen_*` 旧名全部仍可用，返回结果与 `aevum_*`
   **逐字节相同**，写操作日志照常记录。但 `tools/list` 只列新名（见上）。
5. **启动器兼容。** `恒晷.bat` 查找顺序：`dist\aevum\aevum.exe` → `dist\lumen\lumen.exe` →
   `target\release\aevum.exe` → `target\debug\aevum.exe`。已解压过 0.4 绿包的人不用改任何东西。
6. **插件不受影响。** 插件清单的 `id` 前缀不被解析，已装的第三方插件照常加载。只有仓库内的
   示例插件改名为 `aevum.example-invert`；若你两个都装过，菜单里会出现两个，删掉旧的即可
   （不会自动改写用户的 `plugin.toml`）。

### 已知问题

- **`scripts/package.sh` 的默认 ffmpeg 路径已失效**（`../pythonProject4/.venv/...`，多数机器上
  不存在；**改名之前就是这样，不是本版引入的**）。本版只改进了失败提示：找不到时会把探测顺序
  和下一步命令打出来然后退出。请显式传 `AEVUM_FFMPEG_SRC=<ffmpeg.exe 绝对路径>`。
- **`lapse/` 目录名属历史遗留**：它是与 Rust 实现逐式同构的 Python 参考实现，与产品品牌无关，
  不随改名变化。
- **clippy / rustfmt 在本机工具链上本来就是红的**（4 条 lint + 197 处格式差异，其中一处落在
  冻结的布局文件内）。本版按设计**只保证差异为零**，不做「强行全绿」。
- **beta 期不保证跨版本兼容**：`0.x` 期间工程文件格式、预设、命令行参数与 MCP 工具集都可能
  继续调整，每次都会在本文件里标明。
- **附着模式（`--mcp --attach`）下有一小部分工具名解析不到**，且 `xmp` 相关工具在两个模式下
  命名不一致（`xmp_read`/`xmp_write` vs `xmp_import`/`xmp_export`）。**这是 0.4 就存在的问题，
  不是本版引入**；下一轮统一命名并补穷举测试。
- **工程落在只读盘上时，界面不会提示**：护带目录搬不动、保存可能失败，但只有命令行 stderr 有
  一行告警，GUI 里看不出来。改动需要界面告警，另开单处理。
- **工程目录下若存在一个名为 `.aevum` 的普通文件，保存会失败**（罕见，通常是用户误建，或别的
  工具写的同名文件）。打开正常、参数正常，但**保存时报 `os error 183`**（「当文件已存在时，无法
  创建该文件」），提示里看不出该怎么办。本版**已保证不会删掉那个文件**（迁移时会跳过、继续用旧
  目录并提示一句），但「没有旧目录、只有那个文件」的情况下仍然保存不了。处理办法：把该文件改名
  或删除后重试。**这是 0.4 就存在的存量缺口，不是本版引入**；下一轮补「护带目录可用性前置检查」
  与可读错误文案，并明确告诉用户数据在哪。
- **便携模式的 override 路径失效时静默回退默认配置**，没有诊断输出，另开单处理。

### 回滚

- **程序**：直接换回 0.4 的 `lumen.exe` / `dist/lumen/` 即可。
- **工程**：`.aevum/` 与 `.lumen/` 的 `project.json` 格式完全相同，把 `.aevum/` 改回 `.lumen/`
  就能被 0.4 打开。
- **配置**：`%APPDATA%/aevum-studio` 改回 `%APPDATA%/lumen-studio`。

> **⚠️ 不要在新旧版本之间来回打开同一个序列。**
>
> 0.5 已经把 `.lumen/` 搬成 `.aevum/` 之后，如果你再用 0.4 的快捷方式打开**同一个序列**：
> 0.4 找不到 `.lumen/`，会**新建一个空的**，于是界面显示默认参数——看起来像参数全丢了，
> 其实都还在 `.aevum/` 里。你随后在 0.4 里做的改动会写进这个**空目录**，再切回 0.5 时
> 0.5 优先读 `.aevum/`，那些改动被**静默忽略**，两边再也对不上。
>
> 要回退就一次性回退：先按上面把目录改回 `.lumen/`（或配置改回），确认 0.4 正常后再继续用
> 0.4。**不要在中间交叉打开**。

### 旧名保留清单（可核验）

下面这些文件里仍然出现 `lumen`，**全部是有意保留**，不是漏改。
共 **6 类、29 个文件、227 处**（口径：`grep -oi lumen` 的**出现次数**，大小写不敏感，
不是行数——所以数字会比你在编辑器里搜到的行数大）。

`lapse/` 与 `oracle/` 里 `lumen` 的命中是 **0**：旧名 `lapse` 只作为**目录名**存在，
不在这套 `lumen` 替换范围内。它们出现在下面的豁免名单里，是因为**整目录豁免**，不是因为
里面还有残留——这两件事别混。

`oracle/` 的门槛单独说清楚：**`oracle/golden/` 零 diff（10 个文件 sha256 全等）；
`oracle/*.py` 只允许注释 / 路径级改动，禁止任何代码与数值改动。** 本版对 `oracle/` 的唯一改动
是 `dehaze_downsample_ref.py` 里一行注释（把一个**已经不存在的老路径**改成新路径），
不碰回来才是错的：那会让一个错路径永久留在仓库里。

| 类 | 文件 | 处数 | 保留理由 |
|---|---|---|---|
| 兼容层定义 | `crates/aevum-core/src/brand.rs`、`env.rs`、`config_dir.rs`、`project.rs`、`journal.rs` | 54 | 旧目录名 / 旧环境变量前缀 / 旧工具名前缀的**唯一真源**。删任一条，旧工程、旧环境变量、旧 MCP 工具名立刻全断。`journal.rs` 里那 19 条旧写操作名尤其关键：漏了不会报错，只会让日志悄悄缺记录 |
| 兼容性测试 | `crates/aevum-core/tests/project_roundtrip.rs`、`export_stills.rs`，`crates/aevum-mcp/tests/tools.rs`、`mcp_stdio.rs`，`crates/aevum-mcp/src/attach.rs`、`server.rs`、`lib.rs` | 59 | 这些 `lumen` 是「旧写法必须还能用」的证词，删了等于删测试 |
| 用户可见说明与迁移工具 / 维护脚本注释 | `README.md`、`CHANGELOG.md`、`scripts/migrate-to-aevum.sh`、`scripts/package.sh`、`tools/fix_visibility.py`、`恒晷.bat`、`.cargo/config.toml` | 94 | 不写旧名，用户与迁移脚本都不知道要搬什么；`恒晷.bat` 那条 `dist\lumen\lumen.exe` 探测是给已解压 0.4 绿包的人留的；`package.sh` 那句「旧名 `LUMEN_FFMPEG_SRC` 已不再读取」是给打包的人看的升级警告；`fix_visibility.py` 那行注释解释的是「为什么把写死的仓库目录改成从 `__file__` 推导」——删了它，下一个改脚本的人会重新写死路径 |
| 环境变量回退承诺的文档注释 | `crates/aevum-ui/src/settings.rs`，`crates/aevum-core/src/queue.rs`、`library.rs`、`lcp.rs`、`export.rs`、`dcp.rs`、`sequence.rs` | 8 | 「（旧名 `LUMEN_*` 回退）」写在类型文档里正好，是给调用方的承诺 |
| 与品牌无关的任意字符串 | `crates/aevum-core/src/exif.rs` | 4 | EXIF 夹具里的**假厂商名** `"LUMEN"`——测试里最省事的任意大写串，和品牌无关，改了只把 diff 做大 |
| 冻结区 / 一次性历史脚本 | `tools/split_step1.json`、`tools/split_step2.json`（另 `oracle/**`、`lapse/**` 为整目录豁免，`lumen` 命中 0） | 8 | 这两个是已经跑完的 UI 拆分脚本的**输入快照**，路径写的是当年的 `lumen-ui`，改了等于篡改记录。`oracle/` 本轮仅注释路径更新，代码与金标零改动 |

两条命令核验（Git Bash）。第一条必须输出 `白名单外：0 个`：

```bash
allowed() { case "$1" in
  lapse/*|docs/*|oracle/*|tools/split_step1.json|tools/split_step2.json|tools/fix_visibility.py) return 0;;
  crates/aevum-core/src/brand.rs|crates/aevum-core/src/env.rs|crates/aevum-core/src/config_dir.rs) return 0;;
  crates/aevum-core/src/project.rs|crates/aevum-core/src/journal.rs|crates/aevum-core/src/sequence.rs) return 0;;
  crates/aevum-core/src/exif.rs|crates/aevum-core/src/queue.rs|crates/aevum-core/src/library.rs) return 0;;
  crates/aevum-core/src/lcp.rs|crates/aevum-core/src/dcp.rs|crates/aevum-core/src/export.rs) return 0;;
  crates/aevum-core/tests/project_roundtrip.rs|crates/aevum-core/tests/export_stills.rs) return 0;;
  crates/aevum-mcp/src/attach.rs|crates/aevum-mcp/src/server.rs|crates/aevum-mcp/src/lib.rs) return 0;;
  crates/aevum-mcp/tests/tools.rs|crates/aevum-mcp/tests/mcp_stdio.rs) return 0;;
  crates/aevum-ui/src/settings.rs|README.md|CHANGELOG.md|scripts/migrate-to-aevum.sh|scripts/package.sh) return 0;;
  恒晷.bat|.cargo/config.toml) return 0;;
  *) return 1;;
esac; }
bad=0
while IFS= read -r f; do
  grep -qi lumen "$f" 2>/dev/null && { allowed "$f" || { echo "白名单外命中：$f"; bad=$((bad+1)); }; }
done < <(git -c core.quotePath=false ls-files -co --exclude-standard)
echo "白名单外：$bad 个"
```

第二条核验上面那张表的「处数」，必须输出 `227 处 / 29 个文件`：

```bash
total=0; files=0
while IFS= read -r f; do
  n=$(grep -oi lumen "$f" 2>/dev/null | wc -l)
  [ "$n" -gt 0 ] && { total=$((total+n)); files=$((files+1)); }
done < <(git -c core.quotePath=false ls-files -co --exclude-standard)
echo "lumen 出现合计：$total 处，涉及 $files 个文件"
```

用 `git ls-files -co` 而不是 `git grep`：`brand.rs` / `env.rs` / `config_dir.rs` / `.cargo/`
都是**未跟踪**文件，`git grep` 只搜索引、会漏掉它们并给出假绿。这条踩过一次。

第三条核验 `oracle/`。先看 python 侧（必须**无输出**——只允许注释行改动）：

```bash
git diff -U0 oracle/ | grep -E '^[+-][^+-]' | grep -v '^[+-][[:space:]]*#'
```

再看金标本体（必须输出 `金标零 diff`）：

```bash
git diff --stat -- oracle/golden/ | grep . && echo "金标被改了" || echo "金标零 diff"
```

第四条核验中文旧名（口径同上，`ls-files -co`，必须输出 `光时 可疑文件：0 个`）：

```bash
bad=0
while IFS= read -r f; do
  case "$f" in README.md|CHANGELOG.md|docs/*|lapse/*) continue;; esac   # 改名对照表必须写旧名
  all=$(grep -o '光时' "$f" 2>/dev/null | wc -l)
  comp=$(grep -oE '曝光时间|测光时|吃光时|高光时|同光时' "$f" 2>/dev/null | wc -l)
  [ "$all" -eq "$comp" ] || { echo "可疑：$f（光时 $all 处，非品牌词 $comp 处）"; bad=$((bad+1)); }
done < <(git -c core.quotePath=false ls-files -co --exclude-standard)
echo "光时 可疑文件：$bad 个"
```

判据不是「有没有 `光时`」，而是「`光时` 是不是**全都落在已知的非品牌词里**」。
中文没有词边界，`曝光时间` / `测光时` / `吃光时` 里都有这两个字；只搜「有没有」会把正常的
中文词判成残留，只搜「有没有残留」又挡不住替换时把它们一起改坏（本轮就坏过 7 处）。

---

## v0.4.0-beta.1

Rust 重写仓库开放后的首个 beta。三行摘要：

- **LR 对齐 UI 轮**：仿 Lightroom 的多面板布局，对齐与去闪的完整交互；
- **算法里程碑轮**：对齐 / 去闪 / HDR / 全景 / 调色主链全部由 Rust 实现，并建立逐式同构的
  Python oracle 与数值金标；
- **性能重构轮**：RAW 解码、预览、导出链路的批量化与缓存化。

细节见仓库历史与主线提交记录。
