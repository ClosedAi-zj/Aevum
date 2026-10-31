# acceptance/ —— 验收仪器

这个目录放的是**判定「数值对不对」的仪器**，不是产品代码：

```
acceptance/
  check.sh           提交门槛：版本一致性 → 格式 → clippy → 全量测试 → 界面自检
  check_version.py   check.sh 的第 0 关（Cargo.toml 单一真源 → lock / README / CHANGELOG）
  reference/         Python 参考实现（与 Rust 逐式同构的 numpy 镜像）
  oracle/            freeze_golden.py 等冻结脚本 + golden/ 里已冻结的金标数据
```

三者的关系是一条单向链：

```
reference/*.py  ──冻结──▶  oracle/golden/*.json  ──比对──▶  crates/aevum-core 的 Rust 实现
   （唯一真源）              （可审计的标尺）              （被验收的对象）
```

`crates/aevum-core/tests/{develop,golden,cube,align,crop}_golden.rs` 与
`xmp_roundtrip.rs` 运行时直接读 `acceptance/oracle/golden/`，所以这个目录**删了测试就跑不了**。
这也是它必须留在版本库、而不能只留在本地的原因。

## 为什么 Python 在这里，却不在产品里

产品是**纯 Rust**：`cargo build -p aevum-studio` 产出 `aevum.exe`，`scripts/package.sh`
只搬 exe、ffmpeg、文档与许可文本。验收时不需要 Python，用户机器上也不需要 Python。

Python 在这里只有一个身份：**裁判**。一个算法要算得对，光有实现不算——
还得有一份在实现之外、可以逐式对照的独立描述，以及在它上面冻结出来的数字。
`reference/` 就是那份描述（同一套公式换一种语言再写一遍，写错的地方会在金标比对时炸出来），
`oracle/golden/` 就是那些数字。裁判不能兼任运动员，所以它不参与 `cargo build`、
不进 `dist/`、不因为「反正没人用」被删掉——它是验收证据本身。

（这也是本目录独立于 `crates/`、`apps/`、`scripts/` 的原因：产品代码与交付脚本的改动
由评审和门槛把关；验收仪器的改动必须走下面那条更重的流程，不该混在同一次机械重构里。）

## 改算法的标准流程

数值路径不能只改一边，改一边就是「拿实现去适配自己的断言」，那等于没验收。

1. **先改 `reference/`**：`grade.py` / `crop.py` / `align.py` … 逐式改到与设想一致。
   这里是唯一真源，改清楚了再谈 Rust。
2. **重冻结金标**：
   ```bash
   ./.venv/Scripts/python.exe acceptance/oracle/freeze_golden.py --develop-only
   ```
   再跑一遍，`git status --short acceptance/` 必须为空——**两遍逐字节一致**才算金标稳定
   （进程池顺序、JPEG 解码差异都会让第二遍漂，那种基准不能拿来判对错）。
3. **改 Rust 对齐金标**：改 `crates/aevum-core`，让 `cargo test` 里的 `*_golden` 全过；
   断言不许为了迁就实现而放宽——验收口径在动手前就冻结。
4. **过客观验收**：`bash acceptance/check.sh` 五关全绿，退出码 0。绿了才算改完，
   评审看的是这条命令的输出，不是"我觉得对了"。

四类东西必须同一次改动里同步：Rust 实现、Python 参考实现、金标数据、验收阈值。缺一个就是漏改。

## 常用命令

需要仓库根的虚拟环境（依赖只有 numpy + opencv，已装好的话不用重建）：

```bash
# 提交门槛（本机默认 --offline；CI 上 CARGO_FLAGS= 允许联网）
bash acceptance/check.sh
CARGO_FLAGS= bash acceptance/check.sh

# 只跑版本一致性（门槛第 0 关）
python acceptance/check_version.py

# 重冻结显影 / cube 金标（合成图，不读素材）
./.venv/Scripts/python.exe acceptance/oracle/freeze_golden.py --develop-only

# 重冻结蒙版金标（不读素材）
./.venv/Scripts/python.exe acceptance/oracle/freeze_golden.py --masks-only

# 全量重冻结（读素材；--frames 指一个帧目录）
./.venv/Scripts/python.exe acceptance/oracle/freeze_golden.py --frames <帧目录> --count 24
```

冻结脚本里的 `ROOT` 取的是 `acceptance/`（`Path(__file__).parent.parent`），
所以 `--out` 默认落 `acceptance/oracle/golden/`，sys.path 也指向 `acceptance/`——
`import reference` 靠的是这个注入，别把脚本单独拷出去跑。

`reference/` 与 `oracle/` 里的 import 一律写 `from reference import …`。历史上这个包
叫 `lapse`（产品改名前的旧名），目录搬进本仓库的验收目录后一并改掉了；
`.lapse_*.json`、`lapse-studio` 这类**用户数据**上的旧名字是兼容层，不在此列，也不许动。
