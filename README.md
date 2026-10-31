# 恒晷 · Aevum Studio

[中文](#中文) | [English](#english)

---

<a id="中文"></a>

## 中文

**恒晷（Aevum Studio）** 是一款用 Rust 编写的桌面修图 / 延时摄影软件：从相机序列照片出发，
一步接一步做到成片 —— 对齐 → 合成 → 裁剪（含运镜）→ 调色 → 配乐 → 运动（变速与运动模糊），
全部在同一个窗口里完成。上一步的产物自动流进下一步；把文件或文件夹拖进窗口，
会直接切到该去的模块。

> 当前处于 **beta 阶段**（`0.5.0-beta.x`）：核心功能已可用并带数值金标回归，
> 界面与算法仍在快速迭代，正式版（1.0）在完整开发完成后发布。
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
- **数值金标**：Python oracle（`lapse/grade.py`）与 Rust 逐式同构，
  47 个显影金标 case + cube/grade 等多套金标钉住像素级一致

### 仓库结构

```
crates/aevum-core/   算法核心：sequence / metrics / deflicker / grade / preview / align / export / interp / project / xmp / journal
crates/aevum-ui/     egui 0.37 界面：state / shell（14 个子模块）/ theme / widgets / controls
crates/aevum-mcp/    Agent 控制面：MCP 工具、stdio JSON-RPC 与 GUI 回环代理
apps/aevum-studio/   入口（--open / --demo / --shot / --module / --selftest / --mcp）
oracle/golden/       Python 冻结的数值金标
lapse/               Python oracle（numpy 逐式镜像 Rust 影调管线）
tools/               审计与开发辅助脚本
scripts/             check.sh（提交门槛）· package.sh（绿色包）· shots.sh（界面截图）
.github/workflows/   ci.yml —— CI 上跑同一个 check.sh
```

> `lapse/` 是与 Rust 实现逐式同构的 Python 参考实现，名字属历史遗留，**不随产品改名**。
> 它是金标的生成器而不是产品代码：改 Rust 数值路径时它要跟着逐式改（见「测试」），
> 但它的包名永远不会出现在用户机器上。

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

### 测试

```bash
cargo test --workspace          # 全量测试（含 47 个显影金标 case）
cargo clippy --workspace        # lint 门禁
```

金标机制：改 Rust 数值路径时，同步修改 `lapse/grade.py`（逐式镜像），再运行
`python oracle/freeze_golden.py --develop-only` 重冻结；连续两遍冻结必须逐字节一致。

### 版本方案

- 开发期使用 **beta 版本号**（当前 `0.5.0-beta.2`），每轮功能/重构推进打 tag
- 完整开发完成后发布正式版（1.0）；beta 期不承诺输出格式与参数持久化的跨版本兼容
- 许可：MIT（见 `LICENSE` 文件）。随二进制分发的第三方组件的许可与义务见
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

> The project is currently in **beta** (`0.5.0-beta.x`): core features work and are
> pinned by numeric golden tests, while UI and algorithms are still evolving fast.
> The stable release (1.0) ships after development is complete.
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
- **Numeric goldens**: a Python oracle (`lapse/grade.py`) mirrors the Rust pipeline
  formula-by-formula; 47 develop golden cases plus cube/grade suites pin bit-level agreement

### Repository layout

```
crates/aevum-core/   Algorithms: sequence / metrics / deflicker / grade / preview / align / export / interp / project / xmp / journal
crates/aevum-ui/     egui 0.37 UI: state / shell (14 submodules) / theme / widgets / controls
crates/aevum-mcp/    Agent control plane: MCP tools, stdio JSON-RPC, GUI loopback proxy
apps/aevum-studio/   Entry point (--open / --demo / --shot / --module / --selftest / --mcp)
oracle/golden/       Python-frozen numeric goldens
lapse/               Python oracle (numpy mirror of the Rust grade pipeline)
tools/               Audit & dev helper scripts
scripts/             check.sh (commit gate) · package.sh (portable build) · shots.sh (UI screenshots)
.github/workflows/   ci.yml — runs the same check.sh on CI
```

> `lapse/` is the Python reference implementation that mirrors the Rust pipeline
> formula-by-formula. Its name is a historical leftover and deliberately does **not**
> follow the product rename: it is the golden-data generator, not product code, and it
> never ships to users.

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

### Testing

```bash
cargo test --workspace          # full suite, incl. 47 develop golden cases
cargo clippy --workspace        # lint gate
```

Golden workflow: when touching a Rust numeric path, mirror the change in `lapse/grade.py`
formula-by-formula, then re-freeze with `python oracle/freeze_golden.py --develop-only`;
two consecutive freezes must be byte-identical.

### Versioning

- Pre-1.0 releases use **beta versioning** (currently `0.5.0-beta.2`), tagged per milestone
- The stable 1.0 release follows once development is complete; during beta, cross-version
  compatibility of output formats and persisted parameters is not guaranteed
- License: MIT (see the `LICENSE` file). Third-party components shipped with the binary are
  covered by `THIRD-PARTY-NOTICES.txt` and `licenses/`: `rawler` is **LGPL-2.1** and is
  statically linked into `aevum.exe`; for a binary with no copyleft code at all, build with
  `cargo build --release --no-default-features` (RAW falls back to the built-in backend,
  behaviour identical to v0.3.0)
