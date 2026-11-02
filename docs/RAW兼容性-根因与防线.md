# RAW 兼容性：根因与防线

**中文** · [English](RAW兼容性-根因与防线.en.md)

> 性质：事故复盘 + 防线说明。不是发布说明，也不是待办清单——未完成的部分在
> [`遗留台账.md`](遗留台账.md) C8。
> 结论先行：**不是「某个格式不支持」，是「把第三方解析器当成了可信代码」。**

## 1. 事故经过

2026-11-02，用户提供 10 个真实相机样张（`E:/test-p`，164 MB）放进
`samples/cameras/`，第一次跑就抓到了：

```
thread '<unnamed>' panicked at rawler-0.7.2/src/bits.rs:246:
index out of bounds: the len is 1024 but the index is 1024
```

现场是 iPhone 的 `IMG_1381.DNG`（LinearRaw）。真正要命的不是这行 panic，而是**它后面
什么都没有**：

- panic 发生在 rawler 自己起的那批 rayon 工作线程里，被库内部的 `scope` 吞掉；
- 主流程照常返回 `Ok`，交出一张**部分未线性化**的图；
- 界面上一切正常——曲线拉得动、导出能出片，只是暗部数据是错的。

崩溃会有人报 bug；静默的脏数据没有。这一类错误的代价要用「用户以为对的那批成片」
来算。

同一条路径在 `rawler 0.8.0` 上的行为（本轮实测，同一台机器、同一个文件）：

```
thread '<unnamed>' panicked at rawler-0.8.0/src/bits.rs:248:
index out of bounds: the len is 1024 but the index is 1024
```

panic 照样发生（panic 钩子照样打到 stderr），但 0.8.0 自己 `catch_unwind` 住了，
返回 `Failed to decode image, possibly corrupt image: Caught a panic while decoding.`。
我们的路由据此**降级到内置路径**，解出机身内嵌的 4032×3024 JPEG 预览，
并在 `RawPath::Preview` 上如实标注——不崩、不返回脏数据、不假装是传感器数据。

## 2. 根本原因（四层，缺一层都出不了这个事故）

### ① 把第三方解析器当可信代码

Rust 的 `Result` 只覆盖作者**想到并返回了错误**的那部分。库里没想到的路径——`bits.rs`
那种「按位宽算下标，直接 `buf[i]`」的读法——越界就是 panic。这不是 rawler 的实现
质量差；这是所有重度优化的二进制解析器的常态，`image`、`png` 也一样。
**信任边界不在「是不是 Rust 写的」，而在「这份字节是谁给的」。**

### ② `=0.7.2` 精确钉版本，把上游的 panic 修复也一起钉死了

`Cargo.toml` 里当时是 `rawler = "=0.7.2"`，理由写得也很正当：0.8 要求 rustc 1.89，
高于当时声明的 MSRV 1.88。代价是上游对同类 panic 的兜底、以及机型表的更新全都拿不到。
**「钉死」在可复现性上是优点，在吃不可信输入时是缺点**：这类库的 bug 修复通常就是
「某个机型/某种压缩不再崩」，而不是新特性。

### ③ panic 发生在工作线程里 → 静默脏数据

这是本事故里唯一**独有**的一层。主线程 panic = 崩溃 = 看得见；工作线程 panic 被
`scope` 吞掉 = 那一格结果没人填，调用方拿到的是「少了一块」的成果物。任何
「并行 + 逐帧独立失败」的架构都有这个面：**必须显式决定「一格失败了之后这一格是什么」**，
否则默认答案是「一个看起来正常的值」。

### ④ 只有一个 CC0 样张，没有厂商矩阵

`samples/cc0/` 里只有一台 iPhone 6s Plus 的 DNG。判据能证明「这一台能解」，
证明不了第二台、第二个厂商、第二种压缩方式。更糟的是**合成了靶子的形状**：
合成靶和实现由同一个人按同一份理解写成，错在同一个地方时两边一起错（2026-11-01 的
CFA 图案映射就是这样活下来的：所有**合规**的 DNG 都被拒绝，只有自家靶子能过）。
真实样张是唯一能打破这个闭环的东西。

## 3. 防线（四道，逐层收窄）

### 3.1 依赖版本：0.8.0，且写清为什么不能再往回

`rawler = "=0.8.0"`，MSRV 随之上抬到 1.89（界面侧只用 1.88 的 let-chain，1.89 不收回
任何已用特性，所以这个上抬没有代价）。`Cargo.toml` 的注释里直接写明「**必须守在
0.8+**」以及原因——下一个想省一次编译的人会先看到它。

### 3.2 统一的 `decode_guard`：不可信字节的每一条入口都过它

`crates/aevum-core/src/raw/guard.rs`，两个函数：

| 函数 | 用在哪 | 为什么是这个形状 |
|---|---|---|
| `decode_guard(what, f)` | 错误词汇已经是 `RawError` 的边界（rawler 的 `decode_file`、容器内嵌 JPEG 解码） | 闭包返回 `Result<T, RawError>`，panic 直接变成 `RawError::DecoderPanic` |
| `catch_decoder_panic(what, f)` | 错误词汇不是 `RawError` 的边界（`metrics` 的普通图解码、缩略图缓存读、导出前问尺寸） | 保留调用方自己的错误类型：一张普通 JPEG 不该被报成「RAW 解码失败」。`decode_guard` 是它的特化 |

已覆盖的边界（五处，全部有注释说明「这里的字节从哪来、为什么不可信」）：

1. `raw/rawler_backend.rs`：`rawler::decode_file`（第三方，且它自己内部还并行）
2. `raw/preview.rs`：容器内嵌 JPEG 的**尺寸探测**（`into_dimensions`）与**解码**
3. `metrics.rs`：普通图 `reader.decode()`（jpge / zune 同样是第三方）
4. `metrics.rs`：缩略图缓存读（文件在用户盘上待过，可能被截断或替换）
5. `export.rs`：导出前问尺寸（只读头部，但一个坏文件不该 panic 掉整次导出）

**外加一层最外层**：`metrics::decode_with_origin` 整个函数体被守卫包住。理由是它是
「路径 → 像素」的**唯一漏斗**（序列解码、测光、缩略图、导出都走它），而漏斗里除了
第三方解码器还有**本仓自己写的** TIFF/DNG 解析器——后者同样吃不可信字节，同样可能
因为没想到的输入 panic。分级的精度仍在各条支路自己的守卫上；最外层只保证
「panic 的后果是这一帧跳过，不是整个序列消失」。

`UnwindSafe` 的边界在模块头写清了：这里全是**只读解码**，解码器在闭包里被消费掉，
panic 之后没有任何半成品被复用。少数持有 `&mut` 的适配器（`ImageReader` 被 move 进
闭包）用 `AssertUnwindSafe` 明确认账，理由同前。

### 3.3 错误分级：说得出名字，别人才知道下一步

| 情况 | 错误 | 用户看到的话 | 下一步 |
|---|---|---|---|
| 后缀认得、这一版没有解码器（HEIC/HEIF + 15 个厂商 RAW 后缀） | `RawError::UnsupportedFormat { ext }` | 「`heic` 格式暂不支持；把样张放进 `samples/cameras/` 可以在后续版本适配」 | 转格式，或把样张给我们 |
| 解码器内部 panic（被守卫收住） | `RawError::DecoderPanic { backend, detail }` | 「rawler 解码器内部出错，这张先跳过（不是你的文件坏了）」 | 跳过这一帧，其余照常 |
| 容器/压缩解不动 | `NotTiff` / `Rawler` / `Image` … | 各自具体的结构描述 | 退预览，或跳过 |
| 布局不认识（能力边界） | `soft_fallback()` 为真的一批 | —— | **退内嵌预览**，`RawPath` 如实标注 |

两张后缀表是两件事，别合并：`RAW_EXTENSIONS`（7 个）表示「**我们有解码器**」，
`UNSUPPORTED_EXTENSIONS`（19 个）表示「**我们认得、也明确不做**」。合并的那一刻，
「暂不支持」就会退化成「格式无法确定」。

### 3.4 样张矩阵：新样张 = 新判据

`samples/cameras/`（文件不入库，清单与 SHA256 见该目录 README）+
`crates/aevum-core/tests/suite/camera_samples.rs`：

- `every_camera_sample_ends_in_a_named_outcome`：每个文件都必须落在
  `Ok`（尺寸 > 0）/ `UnsupportedFormat` / `DecodeFailed` / `DecoderPanic` 之一，
  **整次调用被 `catch_unwind` 包住**——捕获到就红并打印文件名。
  目录缺失时 **skip**（样张不入库是正常状态），但**有文件却没通过永远是红**。
- `heic_is_rejected_by_name_not_guessed`：HEIC 必须按后缀具名拒绝。

判据的形状是有讲究的：**不锁具体错误串，锁「落在哪一类」**。锁字符串会把
image crate 的措辞焊进判据，上游改一句话就要改测试；锁分类只要求「这件事有名字」，
而上游措辞变化本来就该由 `decoder` 那一层的单元判据管。

## 4. 为什么今后不会再出现

四条，按「防线坏了会怎样」排序：

1. **新解码器必须走 guard**。`raw/guard.rs` 的模块头写了「这条边界为什么存在」，
   任何新增的 `第三方解码调用` 都会被 review 追问「守卫在哪」。这一条不靠自觉，
   靠 review 的可读性：五处调用点全部带一句「这里的字节从哪来」。
2. **新样张 = 新判据**。往 `samples/cameras/` 丢一个文件、跑一次判据，就多一条
   「这台机器/这种压缩必须落在一个具名结果上」的断言。**先让判据红，再改代码**——
   反过来（先改代码再找样本）就是这次事故的成因。
3. **判据缺失 skip 而非 fail**。因为样张不入库，「没有样张」是常态，
   不能因为换台机器就把门槛搞红（那会训练所有人忽略红色）。但「有样张却不通过」
   永远是红。
4. **panic 的后果被定义过**。工作线程 panic 不再等于「一格静默的脏数据」：
   要么被守卫收成错误，要么由最外层兜住让这一帧跳过。**没有第三种可能**。

## 5. 已知不支持清单（截至本轮）

| 项 | 现状 | 依据 |
|---|---|---|
| HEIC / HEIF | 具名拒绝（`UnsupportedFormat`） | 判据 `heic_is_rejected_by_name_not_guessed` |
| Fujifilm RAF、Pentax PEF、Samsung SRW、Hasselblad 3FR/FFF、Phase One IIQ、Minolta MRW、Kodak KDC/DCR/DCS/DC2、Sigma X3F、Canon CRW、Mamiya MEF、Leaf MOS、GoPro GPR | 具名拒绝（同一张表，19 个后缀） | `raw::UNSUPPORTED_EXTENSIONS` |
| Sony `arw6`（如 A7V）机型配置 | rawler 0.8 不认识 → 退内嵌预览 | `samples/cameras` 实测 |
| Nikon HighEfficiencyStar 压缩 | rawler 0.8 不支持 → 退内嵌预览 | 同上 |
| iPhone LinearRaw DNG | rawler 内部 panic（已自兜）→ 退内嵌预览 | 同上，本事故现场 |
| 非 TIFF 家族容器的 Sony 小尺寸变体（`IMG_9590.ARW`） | `NotTiff`；缩略图仍可从容器里的整段 JPEG 出 | 同上 |
| 金字塔法（多尺度逐层细化） | 未实现 | 台账 A 区 |

## 6. 从 Photoshop / Lightroom 看到的（有证据的部分）

**证据来源先说清**：下面是结构层面的观察，来自对本轮 ACR/`cr_*.cpp` 模块划分的逆向笔记，**不是本仓重新验证的结果**；本仓能自证的是最后那条——「容器层 per-vendor」在我们自己的数据上同样成立（rawler 认不出 Nikon 的 HighEfficiencyStar 与 Sony 的 `arw6`，这就是 per-model 配置的现实）。

ACR 能「什么都能开」，靠的不是「把每种容器都写一遍」，而是两层分离：

1. **像素语义统一在容器里**：`DNG` 用一套标签把所有厂商的传感器语义规范化，
   再叠上**按机型一份的 `.dcp`**（相机配置文件）。ACR 的通用性主要来自这里——
   机身差异落在数据（`.dcp`）而不是代码分支上。
2. **容器层仍然是 per-vendor 的**：`cr_canon_cr3.cpp`、`cr_read_nikon.cpp` 这类模块
   一个都不少，CR3 的 ISO-BMFF 结构、Nikon 的压缩方式各有各的解析器。

结论（也是这一轮的取舍依据）：

- **容器层无法避免 per-vendor**。CR3 是 BMFF、CRW 是 CIFF、X3F 是 Foveon 三层，
  想用一套代码解完是幻想；
- 能避免的是「**未知格式开不了**」。我们该做的是「未知格式优雅降级 + 样张驱动适配」，
  不是追全厂商支持；
- 因此本轮的动作是：**把失败的形状做对**（具名错误 / 退预览 + 如实标注 / 绝不崩），
  **把「加一个格式」的成本降到一个样张 + 一条判据**，而不是硬啃机型表。

## 7. 这一轮改了什么（一句话版）

`rawler` 0.7.2 → 0.8.0；五条不可信解码边界 + 一层最外层全部过 `catch_unwind`；
错误分三级（暂不支持 / 解码器内部出错 / 具体结构问题）；10 张真实样张 + 两条矩阵判据；
输入加固（插件清单 1 MB 上限、普通图单边 65535 上限、TOML/serde_json 递归限制已确认在）。
