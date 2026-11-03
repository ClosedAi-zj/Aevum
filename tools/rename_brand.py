# -*- coding: utf-8 -*-
"""品牌改名 + 版本重置的一次性迁移接口。

品牌常量的唯一真源是 `crates/aevum-core/src/brand.rs`（运行时全部引用常量）；
但 crate 名、目录名、Cargo.toml、文档这些**结构层**的东西引用不了常量，改名时
必须一并搬。这个脚本把「改名」收敛成一条命令：

    python tools/rename_brand.py --en "Zephyr Studio" --cn "逐光" --bin zephyr
    python tools/rename_brand.py --en "Zephyr Studio" --cn "逐光" --bin zephyr \\
        --reset-version 0.1.0 --apply     # 加 --apply 才真写；缺省只报告

做什么：
  1. 重写 brand.rs 的全部品牌常量（NAME_EN/NAME_CN/TITLE/BIN/CRATE_PREFIX/
     APP_DIR/GUARD_DIR/ENV_PREFIX/TOOL_PREFIX），并把旧值推进 LEGACY_*：
     旧配置目录、旧护带目录追加进 LEGACY_APP_DIRS / LEGACY_GUARD_DIRS
     （程序首启/开序列时按数组序自动迁移用户数据）；旧环境变量与旧 MCP 工具
     前缀写入 LEGACY_ENV_PREFIX / LEGACY_TOOL_PREFIX（单值回退，见下方警告）。
  2. 全仓文本替换（git ls-files 的文本文件）：`Aevum Studio`→新英文名、
     `Aevum`→新英文单词、`AEVUM`→大写、`aevum_`→`{bin}_`（crate 标识、MCP
     工具名、配置键）、`aevum-`→`{bin}-`（包名、路径、依赖）、`aevum`→`{bin}`、
     `恒晷`→新中文名。brand.rs 由第 1 步专门处理，不进通用替换（否则会把刚
     写进 LEGACY 的旧名又改掉——用占位符隔开）。
  3. `git mv` 结构目录与文件：crates/aevum-{core,ui,mcp}、apps/aevum-studio、
     scripts/migrate-to-aevum.sh。
  4. `--reset-version X.Y.Z`（可选，迁移新仓库从 0 开始时用）：根 Cargo.toml 的
     workspace 版本、Cargo.lock 四个成员版本、README 中英页首的「当前版本」、
     CHANGELOG 中英顶部插入新的 `## X.Y.Z` 节。改完自动跑
     `acceptance/check_version.py` 对账。

警告（脚本会打印，但先说在前面）：
  - LEGACY_ENV_PREFIX / LEGACY_TOOL_PREFIX 是**单值**回退：写入 `AEVUM_`/`aevum_`
    后，更老的 `LUMEN_`/`lumen_` 回退会被顶掉。要链式回退得把 canonical_tool
    改成遍历数组（手工活，脚本只提醒）。
  - scripts/migrate-to-{bin}.sh 里的手工搬运映射只覆盖写死的旧名；aevum→新名
    的批量搬运由程序自动迁移兜底（LEGACY 数组），但整盘素材先搬一遍仍建议
    在该脚本里补一行映射。
  - 改名后第一次 `cargo check` 是全量重编（crate 名都变了），属预期。

用法摘要：
    --en "Zephyr Studio"   新英文展示名（窗口标题英文侧、README、比色空间描述）
    --cn "逐光"             新中文展示名
    --bin zephyr           新可执行名 / crate 前缀（小写字母数字；缺省取 --en 首词小写）
    --reset-version 0.1.0  可选；版本号重置（配新仓库）
    --apply                真写；缺省 dry-run 只报告
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BRAND = ROOT / "crates/aevum-core/src/brand.rs"

TEXT_EXTS = {
    ".rs", ".toml", ".md", ".sh", ".py", ".json", ".yml", ".yaml",
    ".ps1", ".txt", ".lock", ".css", ".html", ".js",
}

# brand.rs 里要重写的常量：名字 → 取值正则（值是 "…" 字符串字面量）。
CONSTS = {
    "NAME_EN": r'pub const NAME_EN: &str = "([^"]*)";',
    "NAME_CN": r'pub const NAME_CN: &str = "([^"]*)";',
    "BIN": r'pub const BIN: &str = "([^"]*)";',
    "CRATE_PREFIX": r'pub const CRATE_PREFIX: &str = "([^"]*)";',
    "APP_DIR": r'pub const APP_DIR: &str = "([^"]*)";',
    "GUARD_DIR": r'pub const GUARD_DIR: &str = "([^"]*)";',
    "ENV_PREFIX": r'pub const ENV_PREFIX: &str = "([^"]*)";',
    "TOOL_PREFIX": r'pub const TOOL_PREFIX: &str = "([^"]*)";',
}


def sh(*args):
    return subprocess.run(args, capture_output=True, text=True, cwd=ROOT, check=True).stdout


def tracked_text_files():
    # -z + quotepath=off：git 默认把非 ASCII 路径转义成八进制（中文文件名会变成
    # "docs/\351..."），Path 直接当真会 is_file()=False，整批中文文档被**静默跳过**
    #（隔离 worktree 验证真踩过：291 行残留全是中文名文件）。
    out = sh("git", "-c", "core.quotepath=off", "ls-files", "-z")
    for line in out.split("\0"):
        if not line:
            continue
        p = Path(line)
        if p.suffix.lower() in TEXT_EXTS and p.is_file():
            yield p


def parse_brand():
    s = BRAND.read_text(encoding="utf-8")
    vals = {}
    for name, pat in CONSTS.items():
        m = re.search(pat, s)
        if not m:
            raise SystemExit(f"rename_brand: brand.rs 里找不到 {name}")
        vals[name] = m.group(1)
    return s, vals


def rewrite_brand(s, old, new):
    """重写 brand.rs：新常量 + 旧值进 LEGACY + 测试字面量占位。

    旧值先写成 ⟦OLD_*⟧ 占位符躲过通用替换，最后一步再换成真值——否则通用
    替换会把刚写进 LEGACY 的旧名又改掉。
    """
    p = lambda v: v  # 占位符直通
    o = old
    # 1) 常量值换成新值
    s = s.replace(f'pub const NAME_EN: &str = "{o["NAME_EN"]}"',
                  f'pub const NAME_EN: &str = "{new["en"]}"')
    s = s.replace(f'pub const NAME_CN: &str = "{o["NAME_CN"]}"',
                  f'pub const NAME_CN: &str = "{new["cn"]}"')
    s = s.replace(f'pub const BIN: &str = "{o["BIN"]}"',
                  f'pub const BIN: &str = "{new["bin"]}"')
    s = s.replace(f'pub const CRATE_PREFIX: &str = "{o["CRATE_PREFIX"]}"',
                  f'pub const CRATE_PREFIX: &str = "{new["bin"]}-"')
    s = s.replace(f'pub const APP_DIR: &str = "{o["APP_DIR"]}"',
                  f'pub const APP_DIR: &str = "{new["app_dir"]}"')
    s = s.replace(f'pub const GUARD_DIR: &str = "{o["GUARD_DIR"]}"',
                  f'pub const GUARD_DIR: &str = "{new["guard_dir"]}"')
    s = s.replace(f'pub const ENV_PREFIX: &str = "{o["ENV_PREFIX"]}"',
                  f'pub const ENV_PREFIX: &str = "{new["env_prefix"]}"')
    s = s.replace(f'pub const TOOL_PREFIX: &str = "{o["TOOL_PREFIX"]}"',
                  f'pub const TOOL_PREFIX: &str = "{new["tool_prefix"]}"')
    s = s.replace(f'pub const TITLE: &str = "{o["NAME_CN"]} · {o["NAME_EN"]}"',
                  f'pub const TITLE: &str = "{new["cn"]} · {new["en"]}"')
    # 2) LEGACY 数组：旧值插到最前（程序按「从新到旧」探测），去重，数组长度随内容长
    def grow_array(text, const_name, new_first):
        m = re.search(r'pub const %s: \[&str; (\d+)\] = \[([^\]]*)\];' % const_name, text)
        if not m:
            raise SystemExit(f"rename_brand: 找不到 {const_name}")
        n, body = int(m.group(1)), m.group(2)
        items = [x.strip().strip('"') for x in body.split(",") if x.strip()]
        items = [new_first] + [x for x in items if x != new_first]
        arr = ", ".join(f'"{x}"' for x in items)
        return text[:m.start()] + f'pub const {const_name}: [&str; {len(items)}] = [{arr}];' + text[m.end():]

    s = grow_array(s, "LEGACY_APP_DIRS", f"⟦OLD_APP⟧")
    s = grow_array(s, "LEGACY_GUARD_DIRS", f"⟦OLD_GUARD⟧")
    # 3) 单值旧前缀：直接换成刚退位的名字（占位符防通用替换）
    s = re.sub(r'pub const LEGACY_ENV_PREFIX: &str = "[^"]*";',
               f'pub const LEGACY_ENV_PREFIX: &str = "⟦OLD_ENV⟧";', s)
    s = re.sub(r'pub const LEGACY_TOOL_PREFIX: &str = "[^"]*";',
               f'pub const LEGACY_TOOL_PREFIX: &str = "⟦OLD_TOOL⟧";', s)
    # 4) canonical_tool 的测试字面量：旧工具名输入要用「刚退位的工具前缀」
    s = s.replace('"lumen_grade_get"', '"⟦OLD_TOOL⟧grade_get"')
    s = s.replace('"lumen_open_sequence"', '"⟦OLD_TOOL⟧open_sequence"')
    s = s.replace('"lumen_"', '"⟦OLD_TOOL⟧"')
    return s


def generic_replacements(old, new):
    """通用替换表。顺序即优先级：长短语法先于短语法，避免二次改写。"""
    return [
        (old["NAME_EN"], new["en"]),
        (old["NAME_EN"].split()[0] if " " in old["NAME_EN"] else old["NAME_EN"], new["word"]),
        (old["ENV_PREFIX"].rstrip("_"), new["word"].upper()),
        (old["TOOL_PREFIX"], new["tool_prefix"]),
        (old["CRATE_PREFIX"], f"{new['bin']}-"),
        (old["BIN"], new["bin"]),
        (old["NAME_CN"], new["cn"]),
    ]


def main():
    ap = argparse.ArgumentParser(add_help=True)
    ap.add_argument("--en", required=True, help='新英文展示名，如 "Zephyr Studio"')
    ap.add_argument("--cn", required=True, help="新中文展示名")
    ap.add_argument("--bin", default=None, help="新可执行名/crate 前缀（缺省取 --en 首词小写）")
    ap.add_argument("--reset-version", default=None, dest="reset_version")
    ap.add_argument("--apply", action="store_true", help="真写；缺省 dry-run")
    args = ap.parse_args()

    en = args.en.strip()
    cn = args.cn.strip()
    word = re.sub(r"[^A-Za-z0-9]", "", en.split()[0])
    bin_name = args.bin or word.lower()
    if not re.fullmatch(r"[a-z][a-z0-9]*", bin_name):
        raise SystemExit(f"rename_brand: --bin 必须是 [a-z][a-z0-9]*，得到 {bin_name!r}")
    env_prefix = f"{bin_name.upper()}_"
    new = {
        "en": en, "cn": cn, "word": word, "bin": bin_name,
        "app_dir": f"{bin_name}-studio",
        "guard_dir": f".{bin_name}",
        "env_prefix": env_prefix,
        "tool_prefix": f"{bin_name}_",
    }

    brand_src, old = parse_brand()
    print(f"旧名：EN={old['NAME_EN']!r} CN={old['NAME_CN']!r} bin={old['BIN']!r} "
          f"app_dir={old['APP_DIR']!r} guard={old['GUARD_DIR']!r} env={old['ENV_PREFIX']!r} "
          f"tool={old['TOOL_PREFIX']!r}")
    print(f"新名：EN={en!r} CN={cn!r} bin={bin_name!r} app_dir={new['app_dir']!r} "
          f"guard={new['guard_dir']!r} env={new['env_prefix']!r} tool={new['tool_prefix']!r}")

    repl = generic_replacements(old, new)
    # 占位符必须躲过通用替换：确认新旧值都不含 ⟦ ⟧
    for k, v in {**old, **new}.items():
        assert "⟦" not in str(v), k

    files = list(tracked_text_files())
    plan = {}   # path -> (替换总次数)
    total = 0
    for p in files:
        text = p.read_text(encoding="utf-8")
        if p.name == "brand.rs":
            continue
        n = 0
        for a, b in repl:
            n += text.count(a)
            text = text.replace(a, b)
        if n:
            plan[p] = n
            total += n
    brand_n = sum(brand_src.count(a) for a, _ in repl)
    print(f"\n将改动 {len(plan)} 个文件共 {total} 处；brand.rs 另有 {brand_n} 处（走专门改写）。")

    # 结构改名清单
    moves = []
    for d in ["crates/aevum-core", "crates/aevum-ui", "crates/aevum-mcp", "apps/aevum-studio"]:
        nd = str(d).replace("aevum", bin_name)
        if Path(d).is_dir() and not Path(nd).exists():
            moves.append((d, nd))
    if (ROOT / "scripts/migrate-to-aevum.sh").is_file():
        moves.append(("scripts/migrate-to-aevum.sh", f"scripts/migrate-to-{bin_name}.sh"))
    for a, b in moves:
        print(f"  git mv {a} → {b}")

    # 版本重置计划
    if args.reset_version:
        ver = args.reset_version
        print(f"版本重置 → {ver}：根 Cargo.toml、Cargo.lock 四个成员、README 中英页首、"
              f"CHANGELOG 中英顶部插入新节。")

    if not args.apply:
        print("\n（dry-run：没写任何文件。确认无误后加 --apply 执行。）")
        return

    # ---- 真写 ----
    # 1) brand.rs：专门改写 → 通用替换（占位符免疫）→ 还原占位符
    s = rewrite_brand(brand_src, old, new)
    for a, b in repl:
        s = s.replace(a, b)
    s = (s.replace("⟦OLD_APP⟧", old["APP_DIR"])
          .replace("⟦OLD_GUARD⟧", old["GUARD_DIR"])
          .replace("⟦OLD_ENV⟧", old["ENV_PREFIX"])
          .replace("⟦OLD_TOOL⟧", old["TOOL_PREFIX"]))
    BRAND.write_text(s, encoding="utf-8")
    print("brand.rs 已重写")

    # 2) 其余文本
    for p, _ in plan.items():
        text = p.read_text(encoding="utf-8")
        for a, b in repl:
            text = text.replace(a, b)
        p.write_text(text, encoding="utf-8")
    print(f"{len(plan)} 个文本文件已替换")

    # 3) 结构改名
    for a, b in moves:
        sh("git", "mv", a, b)
        print(f"git mv {a} → {b}")

    # 4) 版本重置
    if args.reset_version:
        ver = args.reset_version
        ct = ROOT / "Cargo.toml"
        t = ct.read_text(encoding="utf-8")
        t = re.sub(r'(?m)^version = "[^"]+"', f'version = "{ver}"', t, count=1)
        ct.write_text(t, encoding="utf-8")
        lock = ROOT / "Cargo.lock"
        t = lock.read_text(encoding="utf-8")
        # 包名此时已被通用替换改成新前缀，用新名匹配。
        for member in tuple(f"{bin_name}-{m}" for m in ("core", "ui", "mcp", "studio")):
            t = re.sub(r'(name = "%s"\nversion = ")[^"]+(")' % re.escape(member),
                       rf"\g<1>{ver}\g<2>", t)
        lock.write_text(t, encoding="utf-8")
        for readme, pat in (("README.md", r"(\*\*当前版本\*\*：`)[^`]+(`)"),
                            ("README.en.md", r"(\*\*Current version\*\*: `)[^`]+(`)")):
            p = ROOT / readme
            t = p.read_text(encoding="utf-8")
            t = re.sub(pat, rf"\g<1>{ver}\g<2>", t, count=1)
            p.write_text(t, encoding="utf-8")
        # 「版本方案」段里的第二处（README.md:158 / README.en.md:184）。
        for readme, pat in (("README.md", r"(版本号以 `Cargo\.toml` 为唯一真源，当前为 `)[^`]+(`)"),
                            # 英文 README 里 `1.5.1` 换行在 "is" 之后，正则要跨行。
                            ("README.en.md", r"(the current release is\s+`)[^`]+(`)")):
            p = ROOT / readme
            t = p.read_text(encoding="utf-8")
            t2 = re.sub(pat, rf"\g<1>{ver}\g<2>", t, count=1)
            if t2 == t:
                print(f"  ⚠ {readme}：版本方案段的版本号没匹配上，请手工核对")
            p.write_text(t2, encoding="utf-8")
        # 节标题照既有格式带 v 前缀（`## v1.5.1 — 2026-11-03`），不带 v 会被
        # check_version 当成「最新版本节缺失」。
        for cl, heading in (("CHANGELOG.md", f"## v{ver} — 未发布"),
                            ("CHANGELOG.en.md", f"## v{ver} — unreleased")):
            p = ROOT / cl
            t = p.read_text(encoding="utf-8")
            anchor = t.find("---")
            if anchor < 0:
                anchor = 0
            cut = t.find("\n", t.find("\n", anchor) + 1) if "\n" in t[anchor+3:] else len(t)
            # 在第一个 `---` 分隔线之后插入新节
            line_end = t.index("\n", anchor)
            t = t[:line_end+1] + f"\n{heading}\n\n（{cn} · {en} 新仓库首版。）\n" + t[line_end+1:]
            p.write_text(t, encoding="utf-8")
        print(f"版本已重置为 {ver}")

    # 5) 收尾校验
    print("\n==> 残留检查（旧名还出现在哪）")
    r = subprocess.run(["git", "grep", "-in", old["BIN"], "--", "*.rs", "*.toml", "*.md"],
                       cwd=ROOT, capture_output=True, text=True)
    residue = [l for l in r.stdout.splitlines() if "brand.rs" not in l]
    if residue:
        print(f"  {len(residue)} 行仍含旧 bin 名（brand.rs 的 LEGACY 留档除外）；前 10 行：")
        for l in residue[:10]:
            print("   ", l)
        print("  历史记录（CHANGELOG/台账的旧版本节）出现旧名属正常，按需取舍。")
    else:
        print("  干净（brand.rs 的 LEGACY 留档除外）。")
    try:
        out = subprocess.run([sys.executable, "acceptance/check_version.py"], cwd=ROOT,
                             capture_output=True, text=True)
        print("==> check_version:", out.stdout.strip() or out.stderr.strip())
    except Exception as e:  # noqa: BLE001
        print(f"check_version 跑不了：{e}（重置版本后请手工对账）")
    print("\n下一步：cargo check（首次全量重编，属预期）→ 六关 → 提交。")


if __name__ == "__main__":
    main()
