# 插件目录

这个目录**不是**运行时会读的目录，是一份可以直接抄的样板。程序读的是
`%APPDATA%\lumen-studio\plugins`（`LUMEN_PLUGINS` 可以改到别处）。

想看效果，把这一整个目录指给程序：

```bash
LUMEN_PLUGINS=D:/work/project/pycharm/p/lapse-studio/plugins cargo run --bin lumen -- --demo
```

打开「插件」菜单，最下面会多出「示例」组。

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
id = "lumen.example-invert"   # 反向域名，全局唯一；市场靠它判断"同一插件的哪个版本"
name = "示例：反相"
version = "1.0"
vendor = "Lumen"
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
