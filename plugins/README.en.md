# plugins/

[中文](README.md) · **English**

This directory is **not** read at runtime — it is a sample you can copy verbatim. The program
reads `%APPDATA%\aevum-studio\plugins` (`AEVUM_PLUGINS` points it elsewhere).

To see it in action, hand this whole directory to the program:

```bash
AEVUM_PLUGINS=<repo>/plugins cargo run --bin aevum -- --demo
```

Open the "Plugins" (插件) menu and an extra "Sample" (示例) group appears at the bottom.
See "Making the sample actually run" below before you click it — the repository ships the
manifest, not the binary.

## One plugin is one directory

```
plugins/
  example-invert/
    plugin.toml        ← manifest, must carry exactly this name
    bin/invert.exe     ← entry point (`entry` points at it, relative to the manifest)
```

The directory name is arbitrary — the host goes by the `id` in the manifest, not by the folder.

## Manifest fields

```toml
[plugin]
id = "aevum.example-invert"   # reverse domain, globally unique; the market uses it to tell versions apart
name = "Sample: invert"
version = "1.0"
vendor = "Aevum"
kind = "filter"               # filter | decoder | export
entry = "bin/invert.exe"      # empty string = host-internal implementation, no process started
description = "Inverts every pixel; used to exercise the host protocol"

[menu]
path = "plug/example"         # hangs under the "Sample" group of the "Plugins" menu
label = "Invert…"             # falls back to `name` when empty

[[params.item]]
key = "strength"
label = "Strength"
min = 0.0
max = 100.0
default = 100.0
decimals = 0
unit = "%"
```

## Making the sample actually run

**The manifest is committed; the binary is not.** `bin/invert.exe` is a placeholder path, so
clicking "Invert…" straight after cloning gives you "plugin entry does not exist"
(「插件入口不存在：…\bin\invert.exe」) — the menu entry is real, the executable behind it is
yours to build. The reference implementation is `crates/aevum-core/examples/invert.rs`
(63 lines: handshake, tile loop, cancel, exit):

```bash
cargo build --examples
mkdir -p plugins/example-invert/bin
cp target/debug/examples/invert.exe plugins/example-invert/bin/
```

Now the same command as above really inverts pixels, and the strength slider is live. The same
example is what `crates/aevum-core/src/plugin_host.rs` drives in its cross-process tests — the
protocol you read there is the protocol the host speaks, not a simplified version of it.

Two consequences worth knowing:

- If you would rather not copy binaries around, point `entry` at any absolute-or-relative path
  of your own build; nothing else in the manifest changes.
- An empty `entry` is legal and means "the host implements this itself". That route is only
  wired up for built-in decoders today; a user plugin with an empty `entry` reports
  "provided by the host and not wired up yet" (「由宿主内部实现，还没接上」) instead of failing
  silently.

## Three lines you must not cross

1. **`id` must be a reverse domain** (it contains a dot). An id without a dot is rejected at
   load time, and the error shows up under "manifest problems" in the plugin market.
2. **The first segment of the menu path must be** `file` / `edit` / `develop` / `view` /
   `module` / `plug` / `help`. Anything else does not make the plugin disappear — it lands under
   "Plugins · Other", because "installed but invisible" hurts far more than "in the wrong place".
3. **Filters must not hang off `file`.** Users read those entries as file operations, so the host
   re-hangs them under `plug/<second segment>`.

## Parameter table

`[[params.item]]` keys must be unique within a plugin, `min < max`, `default` must fall inside
the range, and `decimals` is capped at 4. All of this is validated at load time: a manifest that
breaks a rule is listed as broken rather than run with bad values.
