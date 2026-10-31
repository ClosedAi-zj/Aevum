# 恒晷 · Aevum Studio

[中文](#中文) | [English](#english)

---

<a id="中文"></a>

## 中文

**恒晷（Aevum Studio）** 是一款用 Rust 编写的桌面修图 / 延时摄影软件：从相机序列照片出发，
一步接一步做到成片 —— 对齐 → 合成 → 裁剪（含运镜）→ 调色 → 配乐 → 运动（变速与运动模糊），
全部在同一个窗口里完成。上一步的产物自动流进下一步；把文件或文件夹拖进窗口，
会直接切到该去的模块。

> **当前版本**：`1.0.0`（正式版）——核心功能可用并带数值金标回归，从本版起执行下面
> 「版本方案」里的四类兼容承诺（工程格式 / 预设与参数 / 命令行 / MCP 工具集）。
> 逐版改动与已知限制见 `docs/发布说明-v1.0.0.md` 与 `CHANGELOG.md`。
>
> `0.5.0` 起产品由「光时 / Lumen Studio」更名为「恒晷 / Aevum Studio」。
> 旧工程的 `.lumen/` 护带目录、旧环境变量 `LUMEN_*`、旧 MCP 工具名 `lumen_*`
> 都仍然可用（见下面的「从旧版本升级」）。

### 功能特性

- **序列处理管线**：帧对齐（亚像素平移）、去闪（亮度曲线校正）、HDR 合并、全景拼接
- **显影 / 调色**：对齐 Lightroom 经典口径的影调链（曝光 / 曲线 / HSL / 分离色调 / 锐化 / 降噪 / 颗粒 / 暗通道去雾），
  蒙版（线性 / 径向 / 画笔）与局部调整
- **成片**：静帧与视频导出、渲染队列、精确输出尺寸、水印、变速与运动模糊（ffmpeg 管线直推，不留临时帧）
- **AI Agent 控制面**：内置 MCP stdio server（直连 34 个 `aevum_*` 工具），也可通过本地回环 HTTP
  把 MCP 调用代理给已打开的 GUI（附着模式，暴露 GUI 的全部 72 条命令，界面实时更新）
- **数值金标**：Python 参考实现（`acceptance/reference/grade.py`）与 Rust 逐式同构，
  47 个显影金标 case + cube/grade 等多套金标钉住像素级一致

### 仓库结构

```
crates/aevum-core/   算法核心：sequence / metrics / deflicker / grade / preview / align / export / interp / project / xmp / journal
crates/aevum-ui/     egui 0.37 界面：state / shell（14 个子模块）/ theme / widgets / controls
crates/aevum-mcp/    Agent 控制面：MCP 工具、stdio JSON-RPC 与 GUI 回环代理
apps/aevum-studio/   入口（--open / --demo / --shot / --module / --selftest / --mcp）
acceptance/          验收仪器：check.sh（提交门槛）· 子目录 reference/（Python 参考实现）与 oracle/（冻结金标）
tools/               审计与开发辅助脚本
scripts/             package.sh（绿色包）· shots.sh（界面截图）· migrate-to-aevum.sh（旧工程迁移）
.github/workflows/   ci.yml —— CI 上跑同一个 acceptance/check.sh
```

> `acceptance/` 是**验收仪器**，不是产品代码：`acceptance/reference/` 是与 Rust 实现逐式同构的
> Python 参考实现，`acceptance/oracle/golden/` 是它冻结出来的数值金标，`check.sh` 是提交门槛。
> 产品是纯 Rust，exe 里不含任何 Python——这个目录既不参与 `cargo build`，也不随包分发。
> 改算法时它跟着逐式改（见「测试」），详见 `acceptance/README.md`。

（设计文档与评审记录在本地 `docs/`，不进版本库；各版本说明见 GitHub Releases。）

### 构建与运行

要求：Rust 1.88+（MSRV）、ffmpeg（可选，导出视频用；查找顺序 `AEVUM_FFMPEG` → exe 同目录 → `tools/` → `PATH`）。

```bash
cargo build --release -p aevum-studio
target/release/aevum.exe --demo
```

| 命令 | 用途 |
|---|---|
| `aevum.exe` | 正常 GUI；同时在 `127.0.0.1` 随机端口开控制服务 |
| `aevum.exe --open <帧目录>` | 打开序列 |
| `aevum.exe --mcp` | 无头 MCP stdio server，不开窗口 |
| `aevum.exe --mcp --attach <control.json>` | 把 MCP 调用代理给已开的 GUI |
| `aevum.exe --selftest` | 无显示器自检（各模块布局形状计数） |
| `aevum.exe --shot <png> --module <模块>` | 程序自己截图后退出（供视觉回归） |

### 从旧版本升级（0.4 及更早）

改名不影响已有的数据和配置，打开旧工程时会自动迁移：

- **工程目录**：`<序列>/.lumen/` 与更早的 `<序列>/.lapse/` 会在打开序列时搬成 `<序列>/.aevum/`。
  旧目录不删；若目标已存在则以 `.aevum` 为准。
- **环境变量**：`AEVUM_*` 优先，未设置时回退读 `LUMEN_*`（首启会在 stderr 提示一行）。
- **配置目录**：`%APPDATA%/lumen-studio`（及更早的 `lapse-studio`）首次启动时并进
  `%APPDATA%/aevum-studio`，旧目录保留。设了 `AEVUM_SETTINGS` / `AEVUM_LIBRARY` /
  `AEVUM_RENDER_QUEUE` / `AEVUM_PLUGINS` 等便携变量时不碰 `%APPDATA%`。
- **MCP 工具名**：规范名已是 `aevum_*`；旧名 `lumen_*` 作为静默别名继续可用，
  但 `tools/list` 只列新名。**升级后请在客户端重新拉取一次 `tools/list`。**

一次搬完整个素材盘可以用 `scripts/migrate-to-aevum.sh <目录>`（缺省只预览，加 `--apply` 才动手）。

### 从 0.5 beta 升级

覆盖安装即可：解压新包、替换旧包目录，没有任何迁移步骤。工程目录（`<序列>/.aevum/`）与
配置目录（`%APPDATA%/aevum-studio`）都不动，旧工程直接打开，参数、评级、标记、关键帧、
运镜照旧。1.0 之后能承诺什么见下面「版本方案」的四类兼容承诺。

### 测试

```bash
bash acceptance/check.sh        # 提交门槛：版本一致性 / fmt / clippy / 全量测试 / 界面自检
cargo test --workspace          # 全量测试（含 47 个显影金标 case）
cargo clippy --workspace        # lint 门禁
```

金标机制：改 Rust 数值路径时，同步修改 `acceptance/reference/grade.py`（逐式镜像），再运行
`./.venv/Scripts/python.exe acceptance/oracle/freeze_golden.py --develop-only` 重冻结；
连续两遍冻结必须逐字节一致。改完算法客观判定只看 `bash acceptance/check.sh` 是否全绿。

### 版本方案

版本号以 `Cargo.toml` 为唯一真源，当前为 `1.0.0`。`1.0.0` 是第一个正式版，此后每次修复或
功能推进打一个 tag；`0.x` 阶段（`0.5.0-beta.2` 及更早）每轮功能/重构打 tag，与本版之间
没有迁移步骤。

**`1.0.0` 起进入 1.x 兼容承诺期**——beta 阶段的「不承诺跨版本兼容」到此为止。四类对外契约
按下面的口径维护，任何一条要改都留到下一个主版本：

1. **工程文件格式**（`<序列>/.aevum/project.json`）：1.x 内**只增字段**，新增字段一律带
   `serde(default)`，不删字段、不改已有字段的语义。三个方向同时成立——旧工程能被新版本打开
   （缺的字段取默认值）；新版本写出的工程被旧版本打开时**只丢掉它不认识的新字段**，不报错、
   不拒绝打开；同一份工程在任意两个 1.x 版本之间来回打开不会丢已有参数。
2. **预设 / 参数持久化**：口径与工程格式完全相同。预设与工程共用同一份参数结构
   （`GradeParams`：工程里是 JSON 字段，XMP 预设里是 `crs:` 键），工程内的关键帧、调色副本
   与应用设置 `settings.json` 一并适用——1.x 内只增字段/只增键，缺省即默认值，不删不改语义；
   新版本写的预设或设置，在旧版本里只丢新增的那部分。
3. **命令行参数**：已有参数及其语义在 1.x 内不变；新增参数只追加，不改已有参数的行为；
   弃用先保留——首次使用时在 stderr 打一行提示（带上替代写法）——到**下一个主版本**才移除。
4. **MCP 工具集**：工具名在 1.x 内不做破坏性变更；新增工具只追加，已有工具的名字、入参与
   返回结构不改。`tools/list` 的旧名回退策略照旧：旧名 `lumen_*` 作为静默别名继续可调，
   列表只列 `aevum_*`（见上面「从旧版本升级」）。

**许可**：MIT（见 `LICENSE` 文件）。随二进制分发的第三方组件的许可与义务见
`THIRD-PARTY-NOTICES.txt` 与 `licenses/`：其中 rawler 为 **LGPL-2.1** 且静态链接进
`aevum.exe`，需要完全不含 copyleft 的构建时用 `cargo build --release --no-default-features`
（RAW 走内置后端，行为与 v0.3.0 一致）

---

<a id="english"></a>

## English

**Aevum Studio** (Chinese name: 恒晷) is a desktop photo-finishing / timelapse tool
written in Rust. It takes you from a sequence of camera frames to a finished film in one
window: **align → composite → crop (with camera motion) → color grade → music → motion
(speed ramps & motion blur)**. Each step feeds the next automatically; drop a file or
folder onto the window and it switches to the right module.

> **Current version**: `1.0.0` (stable): core features work and are pinned by numeric golden
> tests, and the four compatibility promises under "Versioning" (project format / presets and
> parameters / command line / MCP tools) take effect from this release. Per-version changes and
> known limitations live in `docs/发布说明-v1.0.0.md` and `CHANGELOG.md`.
>
> As of `0.5.0` the product was renamed from "Lumen Studio" to "Aevum Studio".
> Old projects (`.lumen/`), old `LUMEN_*` environment variables and old `lumen_*` MCP
> tool names keep working — see "Upgrading from 0.4 and earlier" below.

### Features

- **Sequence pipeline**: frame alignment (sub-pixel), deflicker (luminance-curve based),
  HDR merge, panorama stitching
- **Develop / grading**: a tone pipeline aligned with classic Lightroom behavior
  (exposure / curves / HSL / split tone / sharpening / denoise / grain / dark-channel dehaze),
  masks (linear / radial / brush) with local adjustments
- **Output**: stills & video export, render queue, exact output size, watermark,
  retime with motion blur (frames are piped straight into ffmpeg's stdin — no temporary frame files)
- **AI Agent control plane**: a built-in MCP stdio server (34 `aevum_*` tools), plus a
  loopback HTTP proxy that forwards headless MCP calls to a running GUI with live UI updates
- **Numeric goldens**: the Python reference (`acceptance/reference/grade.py`) mirrors the Rust pipeline
  formula-by-formula; 47 develop golden cases plus cube/grade suites pin bit-level agreement

### Repository layout

```
crates/aevum-core/   Algorithms: sequence / metrics / deflicker / grade / preview / align / export / interp / project / xmp / journal
crates/aevum-ui/     egui 0.37 UI: state / shell (14 submodules) / theme / widgets / controls
crates/aevum-mcp/    Agent control plane: MCP tools, stdio JSON-RPC, GUI loopback proxy
apps/aevum-studio/   Entry point (--open / --demo / --shot / --module / --selftest / --mcp)
acceptance/          Acceptance kit: check.sh (commit gate) · subdirs reference/ (Python reference) and oracle/ (frozen goldens)
tools/               Audit & dev helper scripts
scripts/             package.sh (portable build) · shots.sh (UI screenshots) · migrate-to-aevum.sh (legacy migration)
.github/workflows/   ci.yml — runs the same acceptance/check.sh on CI
```

> `acceptance/` is the **acceptance kit**, not product code: `acceptance/reference/` is the
> Python implementation that mirrors the Rust pipeline formula-by-formula,
> `acceptance/oracle/golden/` is the numeric goldens it freezes, and `check.sh` is the commit
> gate. The product is pure Rust and the exe ships no Python — this directory is not part of
> `cargo build` and is never packaged. See `acceptance/README.md`.

(Design docs and review records live in the local `docs/` directory and are not part of
the repository; per-version notes are published via GitHub Releases.)

### Build & run

Requirements: Rust 1.88+ (MSRV); ffmpeg optional (video export; lookup order
`AEVUM_FFMPEG` → next to the exe → `tools/` → `PATH`).

```bash
cargo build --release -p aevum-studio
target/release/aevum.exe --demo
```

| Command | Purpose |
|---|---|
| `aevum.exe` | Normal GUI; also starts a control service on a random `127.0.0.1` port |
| `aevum.exe --open <frame-dir>` | Open a sequence |
| `aevum.exe --mcp` | Headless MCP stdio server (no window) |
| `aevum.exe --mcp --attach <control.json>` | Proxy MCP calls to a running GUI |
| `aevum.exe --selftest` | Headless self-check (per-module layout shape counts) |
| `aevum.exe --shot <png> --module <module>` | Self-screenshot then exit (visual regression) |

### Upgrading from 0.4 and earlier

The rename does not touch your data or configuration; old state is migrated when opened:

- **Project directory**: `<sequence>/.lumen/` and the older `<sequence>/.lapse/` are moved
  to `<sequence>/.aevum/` when the sequence is opened. Old directories are never deleted;
  if the new one already exists, `.aevum` wins.
- **Environment variables**: `AEVUM_*` takes precedence and falls back to `LUMEN_*`
  when unset (a one-line notice is printed to stderr on first use).
- **Config directory**: `%APPDATA%/lumen-studio` (and the older `lapse-studio`) is merged
  into `%APPDATA%/aevum-studio` on first launch; the old directory is kept. Setting any of
  `AEVUM_SETTINGS` / `AEVUM_LIBRARY` / `AEVUM_RENDER_QUEUE` / `AEVUM_PLUGINS` disables the
  merge entirely (portable mode).
- **MCP tool names**: the canonical names are now `aevum_*`; old `lumen_*` names still work
  as silent aliases, but `tools/list` only advertises the new names.
  **Please refresh `tools/list` in your client after upgrading.**

To convert a whole library at once use `scripts/migrate-to-aevum.sh <dir>`
(dry-run by default; pass `--apply` to actually move).

### Upgrading from 0.5 beta

Install over the top: unzip the new build over the old folder — there is nothing to migrate.
Projects (`<sequence>/.aevum/`) and configuration (`%APPDATA%/aevum-studio`) stay where they
are, existing projects open as before, and parameters, ratings, flags, keyframes and camera
moves are untouched. What 1.0 does and does not promise is spelled out under "Versioning" below.

### Testing

```bash
bash acceptance/check.sh        # commit gate: version check / fmt / clippy / tests / selftest
cargo test --workspace          # full suite, incl. 47 develop golden cases
cargo clippy --workspace        # lint gate
```

Golden workflow: when touching a Rust numeric path, mirror the change in
`acceptance/reference/grade.py` formula-by-formula, then re-freeze with
`./.venv/Scripts/python.exe acceptance/oracle/freeze_golden.py --develop-only`;
two consecutive freezes must be byte-identical.

### Versioning

`Cargo.toml` is the single source of truth for the version; the current release is
`1.0.0`. That is the first stable release; from here every fix or feature bump gets a tag,
and there is nothing to migrate from the `0.x` tags.

**Compatibility promises start with `1.0.0`** and hold for the whole 1.x series — the
beta-stage "no cross-version promises" ends there. Four contracts, each maintained under the
terms below; changing any of them waits for the next major version:

1. **Project file format** (`<sequence>/.aevum/project.json`): within 1.x we only *add* fields,
   always with `serde(default)`; no field is removed and no existing field changes meaning. All
   three directions hold — old projects open in new builds (missing fields take defaults); a
   project written by a new build opens in an old build with only the unknown new fields
   dropped, with no error and no refusal to open; and opening the same project in any two 1.x
   builds never loses existing parameters.
2. **Presets and persisted parameters**: exactly the same terms as the project format. Presets
   and projects share one parameter struct (`GradeParams` — JSON fields in the project, `crs:`
   keys in an XMP preset), and keyframes, grade copies and `settings.json` are covered too.
   Within 1.x we only add fields/keys, absent means default, nothing is removed or redefined;
   a preset or settings file written by a new build loses only the added parts in an old build.
3. **Command-line arguments**: existing flags and their meaning do not change within 1.x; new
   flags are additive only and never alter the behaviour of existing ones; a deprecated flag
   keeps working, prints a one-line notice to stderr on first use (naming the replacement), and
   is removed in the **next major version**.
4. **MCP tool set**: no breaking change to tool names within 1.x; new tools are appended, and
   the name, input schema and result shape of existing tools stay as they are. The legacy-name
   fallback for `tools/list` is unchanged: old `lumen_*` names still work as silent aliases
   while the list advertises only `aevum_*` (see "Upgrading from 0.4 and earlier").

**License**: MIT (see the `LICENSE` file). Third-party components shipped with the binary are
covered by `THIRD-PARTY-NOTICES.txt` and `licenses/`: `rawler` is **LGPL-2.1** and is
statically linked into `aevum.exe`; for a binary with no copyleft code at all, build with
`cargo build --release --no-default-features` (RAW falls back to the built-in backend,
behaviour identical to v0.3.0)
