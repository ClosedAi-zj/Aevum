# How the README screenshots are made

**English** · [中文](README-截图.md)

The five images in the README's "Interface" section are taken **by the repository itself**:
nothing was shot by hand or retouched, and running the script regenerates them from the
current code. This page records the samples, the commands and the material constraints for
all five; after changing the UI, re-shoot by following it.

## One command to re-shoot

```bash
cargo build --release
bash scripts/shots-readme.sh
```

The script does three things:

1. copies the four PANA samples from `samples/cameras/` into `target/readme-shots/` to serve
   as one sequence (the staging directory lives under `target/`: it is already gitignored, so
   `git status` stays clean, and the sequence root shown in the info panel is identical on
   every run);
2. calls `aevum.exe --open <staging> --shot assets/shots/<name>.png …` once per shot;
3. applies the **60 KB blank guard** to each (the same line as `scripts/shots.sh`) -- a flat
   one-colour image cannot pass the script, while the dimension and broken-link criteria live
   on the Rust side (`readme_shots.rs`).

## The five shots

| File | Module | Group expanded | Command (one line in `bash scripts/shots-readme.sh`) |
|---|---|---|---|
| `assets/shots/develop.png` | Develop | `focus` (the focus-stacking group scrolled to the top of the panel: five merge groups plus the depth map / weighted average / pyramid chips) | `--module develop --shot-group focus` |
| `assets/shots/sequence.png` | Sequence | default | `--module sequence` |
| `assets/shots/deflicker.png` | Deflicker | `--shot-channels` (ticks "per-channel (RGB flicker)" to show flicker being suppressed) | `--module deflicker --shot-channels` |
| `assets/shots/compose.png` | Frame | default | `--module frame` |
| `assets/shots/render.png` | Render | default | `--module render` |

Every command has this shape (`<staging>` is `target/readme-shots`):

```bash
target/release/aevum.exe --open <staging> --shot assets/shots/develop.png --module develop --shot-group focus
```

## Samples (all four are PANA, from `samples/cameras/`)

| File | Role |
|---|---|
| `PANA0001.RW2` | frame 1 of the staging sequence (night city, develop's first frame) |
| `PANA0450.RW2` | frame 2 |
| `PANA1631.RW2` | frame 3 |
| `PANA0198.JPG` | frame 4 |

Constraints: **these four only**. Never add files to `samples/cameras/`, and never put
fixture images (synthesised demo sequences) into the README -- fixtures are for the tests,
the README is for people. Staging is a **copy**: the script only reads from `samples/` and
never writes there.

## Where the criteria live

`crates/aevum-ui/tests/suite/readme_shots.rs`, two of them:

- **D-C1 assets**: the five PNGs exist, each over 60 KB, `image::image_dimensions()` at least
  1600x900, and every `assets/shots/…` path referenced by README and README.en matches a real
  file (delete or rename one and it goes red);
- **D-C2 reproducible record**: this page lists the samples and commands for all five,
  `scripts/shots-readme.sh` exists and its five `--shot` lines match this page verbatim
  (change the script without this page and it goes red), and the script never writes into
  `samples/`.

## Known limits

- `--shot` needs a real window (eframe) and cannot open in a headless Session 0 / CI; that is
  ledger item R4. A headless environment only runs D-C1's asset-level criteria (they never
  touch a window); content correctness is checked by a person with a display.
- The four PANA files are **different scenes** (not a real focus bracket), so the alignment /
  check-up numbers on screen are real computations without the "same scene, different focus"
  semantics. The README shows the interface itself; it does not claim these frames fuse into
  one all-in-focus image.
