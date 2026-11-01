# 插件目录

**中文** · [English](README.en.md)

这个目录**不是**运行时会读的目录，是一份可以直接抄的样板。程序读的是
`%APPDATA%\aevum-studio\plugins`（`AEVUM_PLUGINS` 可以改到别处）。

想看效果，把这一整个目录指给程序：

```bash
AEVUM_PLUGINS=<仓库路径>/plugins cargo run --bin aevum -- --demo
```

打开「插件」菜单，最下面会多出「示例」组。点之前先看下面那节「让样例真跑起来」——
仓库里只有清单，没有那个 exe。

## 一个插件就是一个目录

```
plugins/
  example-invert/
    plugin.toml        ← 清单，必须叫这个名字
    bin/invert.exe     ← 入口（`entry` 指向它，相对清单所在目录）
```

目录名随便叫，宿主管它叫什么都不影响 —— 认的是清单里的 `id`。

## 清单字段

```toml
[plugin]
id = "aevum.example-invert"   # 反向域名，全局唯一；市场靠它判断"同一插件的哪个版本"
name = "示例：反相"
version = "1.0"
vendor = "Aevum"
kind = "filter"               # filter | decoder | export
entry = "bin/invert.exe"      # 空串 = 宿主内部实现，不起进程
description = "把每个像素反相，用来验证宿主协议"

[menu]
path = "plug/example"         # 挂在「插件」菜单的「示例」组下
label = "反相…"                # 留空就用 name

[[params.item]]
key = "strength"
label = "强度"
min = 0.0
max = 100.0
default = 100.0
decimals = 0
unit = "%"
```

## 让样例真跑起来

**仓库里入库的只有清单，没有二进制。** `bin/invert.exe` 是个占位路径，所以刚克隆完就点
「反相…」会得到「插件入口不存在：…\bin\invert.exe」——菜单项是真的，背后那个可执行文件
得你自己构建。参考实现就在 `crates/aevum-core/examples/invert.rs`（63 行：握手、分块循环、
取消、退出）：

```bash
cargo build --examples
mkdir -p plugins/example-invert/bin
cp target/debug/examples/invert.exe plugins/example-invert/bin/
```

现在上面那条命令就真的会反相像素，强度滑杆也生效了。`crates/aevum-core/src/plugin_host.rs`
的跨进程测试驱动的也是同一个样例——你在那里读到的协议就是宿主实际说的协议，不是简化版。

两个顺带要知道的点：

- 不想来回拷二进制，就把 `entry` 指向你自己构建产物的任意路径（相对或绝对都行），
  清单别的字段一个都不用改。
- `entry` 留空是合法的，含义是「由宿主自己实现」。这条路目前只给内置解码器接通了；
  用户插件写空串会明确报「由宿主内部实现，还没接上」，而不是静默失败。

## 三条不准踩的线

1. **`id` 必须是反向域名**（带点）。没有点的 id 会在加载时被拒，错误会显示在
   插件市场的「清单有问题」里。
2. **菜单路径第一段只能是** `file` / `edit` / `develop` / `view` / `module` /
   `plug` / `help`。写别的不会让插件消失 —— 会落到「插件 · 其他」，因为
   "装了却看不见"比"位置不对"难受得多。
3. **滤镜类不能挂 `file` 下**。挂上去会被用户当成文件操作，宿主会把它改挂到
   `plug/<第二段>`。

## 参数表

`[[params.item]]` 的 `key` 在一个插件里必须唯一，`min < max`，`default` 要落在
区间内，`decimals` 最多 4。这几条都是加载时校验，不合格的清单会被列出来，
不会带着错值跑起来。
