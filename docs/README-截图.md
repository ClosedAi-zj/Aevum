# README 截图怎么来的

**中文** · [English](README-截图.en.md)

README「界面一览」里的五张图由仓库里的脚本**自己截自己**：不是手工拍的，也不是修过的，
跑一遍脚本就能用当前代码重新生成同样的图。这一页记全五条的样张、命令与素材约束，
改了界面之后照着重截即可。

## 一条命令重截

```bash
cargo build --release
bash scripts/shots-readme.sh
```

脚本做三件事：

1. 把 `samples/cameras/` 的四张 PANA 样张拷进 `target/readme-shots/` 当一个序列
   （staging 目录开在 `target/` 下：`.gitignore` 已盖住它，`git status` 保持干净，
   而且界面信息面板显示的序列根路径每次重截都完全一致）；
2. 逐张调 `aevum.exe --open <staging> --shot assets/shots/<名字>.png …`；
3. 每张过一道 **60 KB 空白守卫**（照 `scripts/shots.sh` 的线）——一张只有底色的空图
   过不了脚本这一关，尺寸与断链判据在 Rust 侧（`readme_shots.rs`）。

## 五张图

| 文件 | 模块 | 展开哪个分组 | 命令（`bash scripts/shots-readme.sh` 里的一行） |
|---|---|---|---|
| `assets/shots/develop.png` | 显影 | `focus`（焦点合成组滚到面板顶部：五个合成分组 + 深度图 / 加权平均 / 金字塔三个方法 chip） | `--module develop --shot-group focus` |
| `assets/shots/sequence.png` | 序列 | 缺省 | `--module sequence` |
| `assets/shots/deflicker.png` | 去闪 | `--shot-channels`（勾上「分通道（RGB 色闪）」，拍色闪被压住的状态） | `--module deflicker --shot-channels` |
| `assets/shots/compose.png` | 构图 | 缺省 | `--module frame` |
| `assets/shots/render.png` | 成片 | 缺省 | `--module render` |

每条完整命令都长这样（`<staging>` 即 `target/readme-shots`）：

```bash
target/release/aevum.exe --open <staging> --shot assets/shots/develop.png --module develop --shot-group focus
```

## 样张（四张全是 PANA，来自 `samples/cameras/`）

| 文件 | 用途 |
|---|---|
| `PANA0001.RW2` | staging 的第 1 帧（夜景城市，显影首帧） |
| `PANA0450.RW2` | staging 的第 2 帧 |
| `PANA1631.RW2` | staging 的第 3 帧 |
| `PANA0198.JPG` | staging 的第 4 帧 |

约束：**只用这四张**。不许往 `samples/cameras/` 里加文件，也不许把夹具图（合成生成的
演示序列）放进 README——夹具图是给判据用的，README 是给人看的。staging 是**拷贝**，
脚本对 `samples/` 只读不写。

## 判据在哪

`crates/aevum-ui/tests/suite/readme_shots.rs`，两条：

- **D-C1 资产层**：五张 PNG 存在、每张 > 60 KB、`image::image_dimensions()` ≥ 1600×900、
  README 与 README.en 引用的 `assets/shots/…` 路径与实际文件一一对应（删一张或改名 → 红）；
- **D-C2 可复现记录**：本页列全五条的样张与命令、`scripts/shots-readme.sh` 存在且
  五条 `--shot` 参数与本页逐字一致（改脚本不改本页 → 红），并且脚本不往 `samples/` 写。

## 已知边界

- `--shot` 需要一个真的窗口（eframe），在无显示器的 Session 0 / CI 里开不出来；
  这与台账 R4 同一条。无显示器环境只跑 D-C1 的资产层判据（它们不碰窗口），
  内容正确性由有显示器的人核对。
- 四张 PANA 是**不同场景**的照片（不是真的焦点包围），所以界面里看到的对齐 /
  体检数据是真实计算的结果，只是没有「同一场景不同对焦」的语义。README 展示的是
  界面本身，不承诺这些帧能合成出一张全景深照片。
