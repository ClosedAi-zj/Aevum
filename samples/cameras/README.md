# 相机样张（不入库）

**中文** · [English](README.en.md)

`samples/cc0/` 里的样本是 CC0、可随仓库分发的；**这个目录不一样**：这里的文件是
用户提供的真实相机产出，**部分来源不明、体积大**，因此**不随仓库分发**（`.gitignore`
里整目录忽略，只放行这两份 README）。换一台机器要跑矩阵判据，得手动把文件拷进来。

为什么要专门有这么一层：`samples/cc0/` 的 iPhone DNG 是**一个**机型、**一种**写法。
1.5 候选第一次拿十个真实样张跑，就抓到 `rawler 0.7.2` 在 iPhone 的 LinearRaw DNG 上
读越界 panic——而且 panic 发生在它自己的 rayon 工作线程里，被吞掉，主流程照常返回
`Ok`，拿到的是部分未线性化的脏数据。**一个样张的样本量撑不起「兼容性」这三个字。**
根因与防线见 `docs/RAW兼容性-根因与防线.md`。

## 清单与校验值

SHA256 由 `sha256sum` 现算（换机后先核一遍，确认拿到的是同一批字节）。
「实测结果」全部是**本轮真跑出来的**，命令见本节末尾。

| 文件 | 相机 | 尺寸 | 字节 | SHA256 | 实测结果 |
|---|---|---|---|---|---|
| `Canon - EOS R6 Mark III - RAW (3_2).CR3` | Canon EOS R6 Mark III | 6982×4654 | 34,608,340 | `e491e4bb960961b5fa299361bf698310a80cbe7b15d30d8dad3bb21bc5457dab` | **OK**，rawler 传感器直解（`RawPath::Sensor`） |
| `IMG_1159.HEIC` | iPhone（HEIC 容器） | — | 2,214,202 | `b7ad723562bd3d8b16bde1c62ce866bc71d1c3004843bff075fc3e7b962e3eba` | **UnsupportedFormat**（按后缀具名拒绝，符合预期） |
| `IMG_1381.DNG` | iPhone（LinearRaw） | 4032×3024 | 12,776,150 | `dda4e93555cc3ce8f6978a9a3283a6df4ca8a33231847c30136fc25059a6bf6a` | **OK，但走内嵌预览**：rawler 0.8.0 内部 panic 被它自己兜住并报「possibly corrupt image」，路由降级到内置路径，解出的是机身 JPEG 预览（`RawPath::Preview`）。进程不崩、不返回脏数据 |
| `IMG_9590.ARW` | Sony（1.7 MB 小尺寸变体） | — | 1,700,212 | `b5730ac0499335501eb46ccfa951d6906e522fb6a951b4487dbfda764899b3a4` | **DecodeFailed**：不是 TIFF 家族容器（`NotTiff`），rawler 也认不出机型。缩略图能出——容器里有一段完整 JPEG，走预览扫描 |
| `Nikon - Z6_3 - 8bit compressed (3_2).NEF` | Nikon Z6III | 3984×2656 | 9,169,408 | `1c7be7b9081fcbd8bb75b28a8432531fae4dc56acda6b7f41a32625fb2555600` | **OK，走内嵌预览**：rawler 报 `NEF compression HighEfficencyStar is not supported`，降级内置路径 |
| `PANA0001.RW2` | Panasonic | 6000×4000 | 29,946,368 | `8f89fbd6b52254052b36ea074262c16ad50e38ece42c52806e55938414a5c061` | **OK**，rawler 传感器直解 |
| `PANA0198.JPG` | Panasonic | 6000×4000 | 12,360,704 | `31940d4244df4fbb322da39d4e486b244583396bdbb768772cd7bdb30952511f` | **OK**，普通图片路径（`image` crate） |
| `PANA0450.RW2` | Panasonic | 6000×4000 | 27,850,752 | `1a635cd69b945e729006b5c5e084ddb924e154ca95c5287cda045126df46c140` | **OK**，rawler 传感器直解 |
| `PANA1631.RW2` | Panasonic（竖拍） | 6000×4000 | 29,060,096 | `061f9c513a307aa387a3a4aee66c33ee9061c0355d7a3bff30a7d63ec23f3c0e` | **OK**，rawler 传感器直解（方向按 EXIF 转正） |
| `Sony - ILCE-7M5 - 14bit (3_2).ARW` | Sony A7V | 4608×3072 | 12,103,680 | `8315af2a9889709f4c836dbaadc23d226847e5a0630fda212a9f896d5fe0569f` | **OK，走内嵌预览**：rawler 报 `Unknown camera, model 'ILCE-7M5', mode: 'arw6'`（还没有这台机的配置），降级内置路径 |

复核方法：`cd samples/cameras && sha256sum *`，与本表逐行对照。

## 每组样张测什么

| 样张 | 它唯一能测到的东西 |
|---|---|
| CR3（Canon，34 MB） | **ISO-BMFF 容器 + CRX 压缩**。TIFF 家族之外的容器，方向标记只在容器里，`exif::read` 读不到 |
| NEF（Nikon，8 bit 压缩） | **新压缩方式**（HighEfficiencyStar）：解不动时必须明确降级到预览，不许假装成功 |
| ARW ×2（Sony 两代） | **同一厂商的两种年代**：老一代（`arw6` 前）能解，A7V 的 `arw6` 还不认识——这正是「机型表驱动」的现实边界 |
| RW2 ×3（Panasonic） | **14 位私有打包**，以及竖拍方向（PANA1631 是 3:4 竖幅） |
| DNG（iPhone LinearRaw） | **三通道 LinearRaw + 已知会让 rawler 越界 panic 的文件**：本轮事故的现场，用来锁住「panic 不许穿出去」 |
| HEIC | **明确不支持的负向**：必须报具名的「暂不支持」，不许掉进「格式无法确定」 |
| JPG（PANA0198） | **普通图片路径**没被 RAW 分派抢走（`6000×4000` 直接由 `image` crate 解） |

## 判据

`crates/aevum-core/tests/suite/camera_samples.rs`：

- `every_camera_sample_ends_in_a_named_outcome`：遍历本目录（跳过 README），对每个文件
  跑 `metrics::decode`。**绝不 panic**（整次调用用 `catch_unwind` 包住，捕获到就红并打印
  文件名），结果必须是 `Ok`（尺寸 > 0）或落在三类具名错误之一：
  `UnsupportedFormat` / `DecodeFailed`（容器或压缩解不动）/ `DecoderPanic`。
  目录不存在或为空时打一行说明并 **skip**——样张不入库，这是正常状态，不是失败。
- `heic_is_rejected_by_name_not_guessed`：HEIC 必须是 `UnsupportedFormat` 且串里带 `heic`。

## 入库规矩（与 `samples/cc0/` 相反）

- **文件不入库**，只入库这份 README 与 `.en.md`。
- 新增样张后：更新上表（尺寸/字节/SHA256/实测结果**现测现填**），跑一遍判据。
- 想让某个格式「从不支持变成支持」，先把它放进这里、让它在判据里红一次，
  再动代码——反过来（先改代码再找样本）就是本轮事故的成因。
