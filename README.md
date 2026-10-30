# 光时 · Lumen Studio

[中文](#中文) | [English](#english)

---

<a id="中文"></a>

## 中文

**光时（Lumen Studio）** 是一款用 Rust 编写的桌面修图 / 延时摄影软件：从相机序列照片出发，
一步接一步做到成片 —— 对齐 → 合成 → 裁剪（含运镜）→ 调色 → 配乐 → 运动（变速与运动模糊），
全部在同一个窗口里完成。上一步的产物自动流进下一步；把文件或文件夹拖进窗口，
会直接切到该去的模块。

> 当前处于 **beta 阶段**（`0.4.0-beta.x`）：核心功能已可用并带数值金标回归，
> 界面与算法仍在快速迭代，正式版（1.0）在完整开发完成后发布。

### 功能特性

- **序列处理管线**：帧对齐（亚像素平移）、去闪（亮度曲线校正）、HDR 合并、全景拼接
- **显影 / 调色**：对齐 Lightroom 经典口径的影调链（曝光 / 曲线 / HSL / 分离色调 / 锐化 / 降噪 / 颗粒 / 暗通道去雾），
  蒙版（线性 / 径向 / 画笔）与局部调整
- **成片**：静帧与视频导出、渲染队列、精确输出尺寸、水印、变速与运动模糊（ffmpeg 管线直推，不留临时帧）
- **AI Agent 控制面**：内置 MCP stdio server（34 个 `lumen_*` 工具），也可通过本地回环 HTTP
  把无头 MCP 调用代理给已打开的 GUI，界面实时更新
- **数值金标**：Python oracle（`lapse/grade.py`）与 Rust 逐式同构，
  47 个显影金标 case + cube/grade 等多套金标钉住像素级一致

### 仓库结构

```
crates/lumen-core/   算法核心：sequence / metrics / deflicker / grade / preview / align / export / interp / project / xmp / journal
crates/lumen-ui/     egui 0.37 界面：state / shell（14 个子模块）/ theme / widgets / controls
crates/lumen-mcp/    Agent 控制面：MCP 工具、stdio JSON-RPC 与 GUI 回环代理
apps/lumen-studio/   入口（--open / --demo / --shot / --module / --selftest / --mcp）
oracle/golden/       Python 冻结的数值金标
lapse/               Python oracle（numpy 逐式镜像 Rust 影调管线）
tools/               审计与开发辅助脚本
scripts/             check.sh（提交门槛）· package.sh（绿色包）· shots.sh（界面截图）
.github/workflows/   ci.yml —— CI 上跑同一个 check.sh
```

（设计文档与评审记录在本地 `docs/`，不进版本库；各版本说明见 GitHub Releases。）

### 构建与运行

要求：Rust 1.88+（MSRV）、ffmpeg（可选，导出视频用；查找顺序 `LUMEN_FFMPEG` → exe 同目录 → `tools/` → `PATH`）。

```bash
cargo build --release -p lumen-studio
target/release/lumen.exe --demo
```

| 命令 | 用途 |
|---|---|
| `lumen.exe` | 正常 GUI；同时在 `127.0.0.1` 随机端口开控制服务 |
| `lumen.exe --open <帧目录>` | 打开序列 |
| `lumen.exe --mcp` | 无头 MCP stdio server，不开窗口 |
| `lumen.exe --mcp --attach <control.json>` | 把 MCP 调用代理给已开的 GUI |
| `lumen.exe --selftest` | 无显示器自检（各模块布局形状计数） |
| `lumen.exe --shot <png> --module <模块>` | 程序自己截图后退出（供视觉回归） |

### 测试

```bash
cargo test --workspace          # 全量测试（含 47 个显影金标 case）
cargo clippy --workspace        # lint 门禁
```

金标机制：改 Rust 数值路径时，同步修改 `lapse/grade.py`（逐式镜像），再运行
`python oracle/freeze_golden.py --develop-only` 重冻结；连续两遍冻结必须逐字节一致。

### 版本方案

- 开发期使用 **beta 版本号**（`0.4.0-beta.1` 起），每轮功能/重构推进打 tag
- 完整开发完成后发布正式版（1.0）；beta 期不承诺输出格式与参数持久化的跨版本兼容
- 许可：MIT（见 LICENSE 文件）

---

<a id="english"></a>

## English

**Lumen Studio** (Chinese name: 光时) is a desktop photo-finishing / timelapse tool
written in Rust. It takes you from a sequence of camera frames to a finished film in one
window: **align → composite → crop (with camera motion) → color grade → music → motion
(speed ramps & motion blur)**. Each step feeds the next automatically; drop a file or
folder onto the window and it switches to the right module.

> The project is currently in **beta** (`0.4.0-beta.x`): core features work and are
> pinned by numeric golden tests, while UI and algorithms are still evolving fast.
> The stable release (1.0) ships after development is complete.

### Features

- **Sequence pipeline**: frame alignment (sub-pixel), deflicker (luminance-curve based),
  HDR merge, panorama stitching
- **Develop / grading**: a tone pipeline aligned with classic Lightroom behavior
  (exposure / curves / HSL / split tone / sharpening / denoise / grain / dark-channel dehaze),
  masks (linear / radial / brush) with local adjustments
- **Output**: stills & video export, render queue, exact output size, watermark,
  retime with motion blur (frames are piped straight into ffmpeg's stdin — no temporary frame files)
- **AI Agent control plane**: a built-in MCP stdio server (34 `lumen_*` tools), plus a
  loopback HTTP proxy that forwards headless MCP calls to a running GUI with live UI updates
- **Numeric goldens**: a Python oracle (`lapse/grade.py`) mirrors the Rust pipeline
  formula-by-formula; 47 develop golden cases plus cube/grade suites pin bit-level agreement

### Repository layout

```
crates/lumen-core/   Algorithms: sequence / metrics / deflicker / grade / preview / align / export / interp / project / xmp / journal
crates/lumen-ui/     egui 0.37 UI: state / shell (14 submodules) / theme / widgets / controls
crates/lumen-mcp/    Agent control plane: MCP tools, stdio JSON-RPC, GUI loopback proxy
apps/lumen-studio/   Entry point (--open / --demo / --shot / --module / --selftest / --mcp)
oracle/golden/       Python-frozen numeric goldens
lapse/               Python oracle (numpy mirror of the Rust grade pipeline)
tools/               Audit & dev helper scripts
scripts/             check.sh (commit gate) · package.sh (portable build) · shots.sh (UI screenshots)
.github/workflows/   ci.yml — runs the same check.sh on CI
```

(Design docs and review records live in the local `docs/` directory and are not part of
the repository; per-version notes are published via GitHub Releases.)

### Build & run

Requirements: Rust 1.88+ (MSRV); ffmpeg optional (video export; lookup order
`LUMEN_FFMPEG` → next to the exe → `tools/` → `PATH`).

```bash
cargo build --release -p lumen-studio
target/release/lumen.exe --demo
```

| Command | Purpose |
|---|---|
| `lumen.exe` | Normal GUI; also starts a control service on a random `127.0.0.1` port |
| `lumen.exe --open <frame-dir>` | Open a sequence |
| `lumen.exe --mcp` | Headless MCP stdio server (no window) |
| `lumen.exe --mcp --attach <control.json>` | Proxy MCP calls to a running GUI |
| `lumen.exe --selftest` | Headless self-check (per-module layout shape counts) |
| `lumen.exe --shot <png> --module <module>` | Self-screenshot then exit (visual regression) |

### Testing

```bash
cargo test --workspace          # full suite, incl. 47 develop golden cases
cargo clippy --workspace        # lint gate
```

Golden workflow: when touching a Rust numeric path, mirror the change in `lapse/grade.py`
formula-by-formula, then re-freeze with `python oracle/freeze_golden.py --develop-only`;
two consecutive freezes must be byte-identical.

### Versioning

- Pre-1.0 releases use **beta versioning** (starting at `0.4.0-beta.1`), tagged per milestone
- The stable 1.0 release follows once development is complete; during beta, cross-version
  compatibility of output formats and persisted parameters is not guaranteed
- License: MIT (see the LICENSE file)
