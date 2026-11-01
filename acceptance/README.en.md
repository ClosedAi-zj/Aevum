# acceptance/ — verification instruments

[中文](README.md) · **English**

This directory holds the instruments that judge **whether the numbers are right**. It is not
product code:

```
acceptance/
  check.sh           commit gate: versions → format → clippy → full tests → UI self-check
  check_version.py   gate 0 of check.sh (Cargo.toml as the single source of truth → lock / README / CHANGELOG)
  check_docs.py      gate 0, second half: every tracked document has a Chinese/English twin, cross-linked
  reference/         the Python reference implementation (a numpy mirror, formula by formula)
  oracle/            freeze_golden.py and friends + the frozen golden data under golden/
```

The three form a one-way chain:

```
reference/*.py  ──freeze──▶  oracle/golden/*.json  ──compare──▶  the Rust implementation in crates/aevum-core
 (single source)              (an auditable ruler)              (the thing being verified)
```

`crates/aevum-core/tests/{develop,golden,cube,align,crop}_golden.rs` and `xmp_roundtrip.rs` read
`acceptance/oracle/golden/` at run time, so **delete this directory and the tests stop running**.
That is also why it has to live in the repository rather than only on your machine.

## Why Python lives here and not in the product

The product is **pure Rust**: `cargo build -p aevum-studio` produces `aevum.exe`, and
`scripts/package.sh` copies only the exe, ffmpeg, the documents and the licence texts. Running
the gate needs no Python, and neither does the user's machine.

Python has exactly one role here: **referee**. An algorithm being correct is not established by
having an implementation — you also need an independent description outside that implementation
that can be followed formula by formula, plus numbers frozen from it. `reference/` is that
description (the same formulas written a second time in another language; anywhere the second
writing is wrong, the golden comparison blows up), and `oracle/golden/` holds the numbers. A
referee cannot also be a player, which is why it takes no part in `cargo build`, never enters
`dist/`, and never gets deleted on the grounds that "nobody uses it" — it *is* the evidence.

(That is also why this directory sits apart from `crates/`, `apps/` and `scripts/`: changes to
product code and delivery scripts are governed by review and the gate; changes to the
instruments must go through the heavier process below, and should not ride along in an ordinary
mechanical refactor.)

## The standard way to change an algorithm

A numeric path cannot be changed on one side only — changing one side means "I bent the
implementation until my own assertion passed", which is not verification.

1. **Change `reference/` first**: `grade.py` / `crop.py` / `align.py` … formula by formula, until
   it matches the intent. This is the single source of truth; settle it before touching Rust.
2. **Re-freeze the golden data**:
   ```bash
   ./.venv/Scripts/python.exe acceptance/oracle/freeze_golden.py --develop-only
   ```
   Run it again; `git status --short acceptance/` must be empty — **two runs byte-identical**
   is what makes a golden stable (process-pool ordering and JPEG decoding differences make the
   second run drift, and such a baseline cannot serve as a ruler).
3. **Change Rust to match**: edit `crates/aevum-core` until every `*_golden` test in `cargo test`
   passes. Do not loosen an assertion to accommodate the implementation — the acceptance criteria
   were frozen before you started.
4. **Pass the objective acceptance**: `bash acceptance/check.sh` all green, exit code 0. Green is
   what "done" means; review looks at that command's output, not at "I think it's right".

Four things must move in the same change: the Rust implementation, the Python reference, the
golden data and the acceptance tolerance. Missing one means the change is incomplete.

## Common commands

Freezing or recomputing golden data needs Python dependencies (numpy and opencv only; skip the
rebuild if you already have them). The list is `acceptance/requirements.txt`, and there is
**no requirements.txt at the repository root** — the product is pure Rust, and a dependency list
at the root would read as "building this needs Python":

```bash
python -m venv .venv
.venv/Scripts/python.exe -m pip install -r acceptance/requirements.txt
```

```bash
# the commit gate (offline by default on this machine; CI sets CARGO_FLAGS= to allow the network)
bash acceptance/check.sh
CARGO_FLAGS= bash acceptance/check.sh

# versions only (gate 0)
python acceptance/check_version.py

# documents only (gate 0, second half)
python acceptance/check_docs.py

# re-freeze the develop / cube golden (synthetic images, reads no footage)
./.venv/Scripts/python.exe acceptance/oracle/freeze_golden.py --develop-only

# re-freeze the mask golden (reads no footage)
./.venv/Scripts/python.exe acceptance/oracle/freeze_golden.py --masks-only

# full re-freeze (reads footage; --frames points at one frame directory)
./.venv/Scripts/python.exe acceptance/oracle/freeze_golden.py --frames <frame dir> --count 24
```

`ROOT` inside the freeze scripts is `acceptance/` (`Path(__file__).parent.parent`), so `--out`
defaults to `acceptance/oracle/golden/` and `sys.path` points at `acceptance/` too — that
injection is what makes `import reference` work, so do not copy a script elsewhere and run it.

Imports inside `reference/` and `oracle/` always read `from reference import …`. The package was
historically called `lapse` (the product's pre-rename name) and was renamed when the directory
moved into this repository's acceptance tree. Old names living in **user data** — `.lapse_*.json`,
`lapse-studio` and the like — are the compatibility layer, are not covered by that rename, and
must not be touched.

## Documents (gate 0, second half)

`python acceptance/check_docs.py` checks three things: every tracked `*.md` has an `.en.md` twin
(both directions), each document links to its twin within the first 15 lines, and every local
relative link in a document resolves to a real file. Site links are not checked — the network is
not something a commit gate can control.

How the two editions are laid out: Chinese first, English after, **each a separate file** rather
than one file in two languages, with a switch line at the top of both
(`**中文** · [English](x.en.md)` / `[中文](x.md) · **English**`).

Adding a document means adding both editions. `docs/*` is ignored by default, so a new document
that should ship needs a whitelist rule in `.gitignore` — and that rule has to cover the
`.en.md` twin too.
