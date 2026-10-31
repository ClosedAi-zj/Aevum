# 第三方许可全文

本目录存放随 `aevum.exe` 一同分发的第三方组件许可原文。**组件的用途、版本、
链接方式与本项目履行义务的具体做法，见仓库根目录 `THIRD-PARTY-NOTICES.txt`**；
本文件只负责"哪个文件对应哪个组件"。

下表描述的是**绿色包 zip 里 `licenses/` 目录**的内容。仓库里只有其中一部分（其余在
`assets/` 下按原位置存放，由 `scripts/package.sh` 打包时统一拷进 `licenses/`），
所以"在仓库里 grep 不到某个文件名"是正常的。

| 文件 | 对应组件 | 许可 | 分发形态 |
|---|---|---|---|
| `LGPL-2.1-rawler.txt` | rawler 0.7.2（RAW 解码后端） | LGPL-2.1 | **静态链接进 `aevum.exe`** |
| `OFL-1.1-Noto-Sans-SC.txt` | Noto Sans SC（思源黑体简体中文） | SIL OFL 1.1 | 编译进 `aevum.exe` |
| `OFL-1.1-NotoEmoji.txt` | Noto Emoji | SIL OFL 1.1 | 编译进 `aevum.exe` |
| `OFL-1.1-Inter.txt` | Inter | SIL OFL 1.1 | 编译进 `aevum.exe` |
| `UFL-1.0-Ubuntu-Light.txt` | Ubuntu Light（egui 默认字体之一） | Ubuntu Font Licence 1.0 | 编译进 `aevum.exe` |
| `Hack-Regular.txt` | Hack（egui 默认字体之一） | MIT（含 DejaVu 公有领域、Bitstream Vera） | 编译进 `aevum.exe` |
| `egui-icons.txt` | egui-icons.ttf（emoji-icon-font 的 5 字形子集） | MIT（同 emoji-icon-font） | 编译进 `aevum.exe` |
| `emoji-icon-font-MIT.txt` | emoji-icon-font | MIT | 编译进 `aevum.exe`（`default_fonts` 特性下也可能只带 egui-icons 子集） |
| `ISC-Lucide.txt` | Lucide Icons（`assets/icons/` 下的 SVG） | ISC | 以源码形式随仓库分发，不编进 exe |

> 文件名前缀是**许可编号**而不是组件名，这样一眼能看出每个文件承担的是哪份义务：
> `LGPL-2.1-*` / `OFL-1.1-*` / `UFL-1.0-*` / `ISC-*`，没有前缀的（`Hack-Regular.txt`、
> `egui-icons.txt`、`emoji-icon-font-MIT.txt`）是上游发布时自带的原名，改了反而对不上。
> 打包脚本里有一条路径映射（`assets/fonts/OFL-NotoSansSC.txt` →
> `licenses/OFL-1.1-Noto-Sans-SC.txt`），改文件名时两处要一起改。

> **两份 Noto 的 OFL 文本内容相同**，这不是拷错：它们就是 OFL 1.1 的许可正文
> （不含版权头）。OFL 要求的"版权声明"写在各字体文件自己的 `name` 表里，
> 由字体内嵌携带——本项目没有修改任何字体文件，所以这些声明原样保留在二进制中。
> 分两个文件名只是为了让你一眼看出"这个许可覆盖哪几个组件"。
> Inter 那份（`OFL-1.1-Inter.txt`）多一段 `Copyright 2020 The Inter Project Authors`，
> 是它上游发布时自带的写法。

## 源文件在哪儿（上表的仓库原位置）

| 组件 | 仓库内原位置 | 打包后 |
|---|---|---|
| Inter 字体 | `assets/fonts/OFL-Inter.txt` | `licenses/OFL-1.1-Inter.txt` |
| Noto Sans SC 字体 | `assets/fonts/OFL-NotoSansSC.txt` | `licenses/OFL-1.1-Noto-Sans-SC.txt` |
| Lucide 图标 | `assets/icons/LICENSE-Lucide.txt` | `licenses/ISC-Lucide.txt` |
| rawler / egui 默认字体等 | 本目录 | 原样拷入 |

映射规则写在 `scripts/package.sh` 的「许可文本」一节，本表只是它的说明。
**改一处必须改另一处**——这张表曾经把 `OFL-1.1-Noto-Sans-SC.txt` 写成
`OFL-1.1-NotoSansSC.txt`（少了两个连字符），照着表去包里找会找不到。

## FFmpeg 的 GPLv3 全文为什么不抄进来

本节放在这里是因为它容易被误判成"漏了"。理由：我们分发的是**未修改的独立程序**，
GPLv3 全文（约 35 KB）与源码获取途径已经写在 `THIRD-PARTY-NOTICES.txt` 第 1 节和
随包 `tools/README-ffmpeg.txt` 里。**许可条款要求的是"提供许可与源码信息"，
不是"必须复制全文"**；FFmpeg 官方的分发也是给链接。
若你所在组织要求随包带全文，把 GPLv3 文本放到本目录即可，不影响任何构建。
