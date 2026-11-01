# Changelog

Version history for Aevum Studio (Chinese name: 恒晷). Each entry records only what **users can
perceive**: behaviour changes, compatibility policy, known issues, and how to roll back. Internal
implementation details, test suites and golden data do not belong here.

> **1.x compatibility promise**: from `1.0.0` on, four public contracts — the project file format
> (`.aevum/project.json`), preset and parameter persistence, command-line arguments, and the MCP
> tool set — are maintained under "add only, never change". The exact terms are in the README
> section "Versioning". `0.x` (`0.5.0-beta.2` and earlier) is outside that promise; breaking
> changes from that era are called out per release, with a migration path or a rollback route
> wherever one exists.

---

## v1.0.1 — 2026-11-01

A patch release: fixes only, no new features. **Three of the five fixes are "it happened twice"
or "it hung forever"** — none of them raises an error, they just make the program look broken.
The four public contracts (project format / presets and parameters / command line / MCP tool set)
are untouched; install over the previous version.

### Fixed: a hung plugin no longer leaves the app stuck on "running"

If a plugin process says nothing during the handshake (wrong entry point, crashes on start, or
simply stalls while initialising), the UI used to keep showing that job as "running" forever, and
cancelling did not help — cancelling only raises a flag, and the blocked thread never reaches the
point where that flag is checked. The handshake now has its own 30-second cap: on timeout the
process is killed and an error is returned. A refused handshake (protocol too old, plugin declines)
also cleans the process up instead of leaving a zombie holding the pipe.

### Fixed: a control command that timed out is no longer executed a second time

When a command is sent through the control channel for scripts or automation, and the UI stays
busy for more than 30 seconds, the client receives a timeout. The command used to stay queued and
be executed anyway once the UI freed up — if the client then retried, write commands such as
`export.video`, `queue.run` and `hdr.merge` ran twice, producing two files from one clip or two
processes fighting over the same output. Commands that waited past the limit are now discarded
with an explicit "not executed, to avoid colliding with a retry" reply.

### Fixed: queue rendering uses the parameters from when the item was added, not the old project on disk

After changing the grade following a queue-up, the rendered clip could miss the last few
adjustments — rendering read the project file from disk, and autosave runs every 5 seconds and is
skipped while the UI is busy. The UI promises "each item uses the project parameters from when it
was added; changing the grade later does not require re-queueing", and that is now actually true:
the in-memory project is captured into the item when it is added, and rendering prefers it. Older
queue files still load (no snapshot means falling back to disk, exactly as before).
**Stated cost**: queue files get bigger — each item carries one project snapshot (tens of KB).

### Fixed: a chatty plugin can no longer eat all memory

Messages a plugin sends back are one line of text each. Any length used to be accepted, so a broken
plugin (or a stray debug print) could exhaust the host's memory by streaming bytes without a
newline. A single message is now capped at 4 MB; exceeding it disconnects and kills the plugin.
Pixel frames already had an 8 MB cap — this closes the text half. The plugin side reading host
messages has the same limit, because the protocol is symmetric and guarding one side is not enough.

### Fixed: a crash while saving can no longer lose the whole project

Saving was "write temp file → delete the old file → rename". There is a very short window between
the delete and the rename; losing power or crashing exactly there lost the project. It is now
"write temp file and flush → rename the old file to `.bak` → rename the new file into place":
if any step fails the old file is still there, and the operation rolls back. When loading, a
corrupt main file falls back to `.bak` (the last version known to be good), so half-written JSON
no longer throws the grade away.

### Fixed: three remaining Chinese strings in the English UI

The clip-path row, the "All" filter label, and automatic snapshot naming plus its two status
sentences still showed Chinese in English mode. Filled in (English entries 703 → 706).

### Known limitations

Identical to `1.0.0` — see the nine items in "3. Known limitations" of
`docs/发布说明-v1.0.0.en.md`; none of them is addressed by this release.

---

## v1.0.0 — 2026-10-31

The first stable release. **It is not about more features — it is three things holding at once**:
user work is not lost (a path that cannot be written says so, in readable words, before anything
is attempted), the external promises are explicit (four contracts: project format / presets and
parameters / command line / MCP tools), and delivery is reproducible (one command produces the
package, and the artefact can be verified byte-for-byte). Compared with `0.5.0-beta.2`, **the only
change that touches pixels is the `eps` of multi-scale denoise**; everything else is delivery
plumbing and documentation.

No breaking change to the project file format, config directory, tool names, or command-line
arguments — install over the top.

### Fix: multi-scale denoise was grinding away too much detail

The old formula treated the guided filter's `eps` as a "contrast threshold" rather than a "noise
variance". At σ=0.02 it produced 86× the noise variance, so structures with modest variance
(stars, texture, thin lines) were classified as noise and flattened — what users saw was "turn on
multi-scale and it goes mushy; turn it off and it is clean".

Readings at the time (1024×768, σ=0.02, `nr_luma`=50): variance dropped 74.5%, but MTF was left at
44.6%. In other words the noise really was suppressed by three quarters, at the cost of suppressing
more than half the detail along with it.

Changing to `eps = 0.8·σ̂²/rm`: `eps` returns to variance units, and `1/rm` compensates for scale
(`rm` is the noise-estimation radius; the larger the short edge, the larger the radius, and the
larger the absolute difference the same `eps` demands on a bigger image). The same measuring
position now reads 53.8% / 90.0% — a little less noise reduction, detail preserved.

| Frame size | Before (variance↓ / MTF) | After |
|---|---|---|
| 512×256 (degenerate frame, not used for judgement) | 72.0% / 64.2% | 26.7% / 93.6% (insufficient denoise) |
| **1024×768 (measuring position)** | 74.5% / 44.6% | **53.8% / 90.0%** |
| 2048×1536 | 74.9% / 48.7% | **60.3% / 89.5%** |

**How it looks now**: with "multi-scale" on, more texture survives than before, and a bit more
noise remains. Previously it was "half the detail for cleanliness"; now it grinds exactly as much
as the noise variance calls for. **The cost, stated plainly**: at the same `nr_luma` setting, total
denoise with multi-scale enabled is lower than pure single-scale (1024×768: 53.8% vs 69.4%), in
exchange for 5.5 points of MTF. The single-scale branch was not touched by a single byte in this
release (pinned by a bit-exact fingerprint test).

Known limits: reading multi-scale denoise strength depends on "the two radii really being
separated", and when the short edge is below 390 pixels the coarse and fine radii degenerate into
the same 5×5 kernel (the same operation twice), so readings there cannot be treated as conclusions
about the algorithm; `rm` above 5 (short edge > 1300) is extrapolation, and 3072×2304 was not
measured.

### New: when something cannot be written or read, the UI says so before acting

- **Read-only drives / a guard directory that cannot be moved**: when opening a project or creating
  a sequence, a warning appears in the status bar (click to dismiss), naming the path, the reason,
  and what will happen to your edits. Previously it failed at save time, or silently dropped work.
- **Portable-mode override paths that are invalid** (missing, not writable, occupied by a file of
  the same name): stderr names the path and the reason, every time it fails, and it does not
  silently fall back to the default config — falling back would break the existing promise that
  "setting an override means `%APPDATA%` is never touched". Better to make noise.
- Both kinds of warning travel through the same process-level channel, deduplicated and capped, so
  the status bar does not fill up.

### Delivery

- `scripts/package.sh` produces the package with one command: gate → release → bundled ffmpeg →
  self-test → real export → zip (**with a top-level directory**, containing `LICENSE` and
  `CHANGELOG.md`) → `SHA256SUMS` → unzip and re-verify the self-test shape counts.
- The package now carries the project's own `LICENSE` and `THIRD-PARTY-NOTICES.txt`, not just the
  exe.
- Version consistency became gate step 0: `Cargo.toml` is the single source of truth, and
  `Cargo.lock`, the four README positions and the newest CHANGELOG heading must all agree. This was
  added after the version had actually drifted once.
- The build is pinned by `rust-toolchain.toml` (1.98.0); CI no longer hand-copies a version number.

### Documentation

- The Chinese and English README sections replaced "no compatibility promise during beta" with the
  four 1.x contracts, and each gained an "Upgrading from 0.5 beta" subsection (install over the
  top; project and config directories stay put).
- New `docs/发布说明-v1.0.0.en.md`: **known limitations consolidated in one place**, named per version,
  committed with the tag.

### Known limitations

Nine of them, all collected in section "3. 已知限制" of this release's notes rather than scattered
across CHANGELOG entries. The two most important to know up front: **HDR export (PQ / AVIF 10-bit)
is a reserved entry point only — do not treat it as a working feature**; and **the exe is not
code-signed**, so Windows shows a SmartScreen prompt on first run — use `SHA256SUMS` to verify.

### Rollback

Switch back to `Aevum-Studio-v0.5.0-beta.2-windows-x64.zip`; no need to move the project or config
directories. What you lose is fields written by 1.0 (older versions ignore them, which is exactly
the direction the compatibility promise states); the parameters themselves are not lost.

---

## v0.5.0-beta.2 — 2026-10-31

No new features in this release — it is all fixes and verifiability: two problems that directly
affect finished output (the overshoot clamp on output sharpening never took effect; saving only
reported a system error when `.aevum` was occupied by a file of the same name), two tool-routing
defects in attach mode, and a measurement foundation that makes sharpening parameters objectively
verifiable from here on. **No breaking change to the project file format, tool names, or
command-line arguments** — install over the top.

### Fix: two `xmp` tools were unreachable in attach mode

`aevum --mcp --attach control.json` forwards MCP calls to a running GUI. At that entry point, tool
names are mechanically generated by prefixing GUI command IDs (`xmp.import` → `aevum_xmp_import`),
while in direct mode the same tool is called `aevum_xmp_read`. The reverse lookup is that same
mechanical derivation, so the two names did not recognise each other:

- in attach mode, calling `aevum_xmp_read` / `aevum_xmp_write` → **unknown tool**;
- conversely, `aevum_xmp_import` / `aevum_xmp_export` do not exist in direct mode either.

All four spellings now work (two tool names plus two command IDs), and all of them land on the same
command.

### Improvement: errors no longer conflate two kinds of cause

Five capabilities are only available in direct mode — metering, deflicker analysis, keyframe
add/list/remove, preview rendering, and unified cancel. The GUI control channel has no
corresponding commands, so rejecting them in attach mode is **correct**. But those names really do
exist in direct mode's `tools/list`, so the same client configuration switched over produced the
same "unknown tool" as a typo, and people kept checking their spelling.

Errors now distinguish the two:

- `aevum_measure` → "only available in server mode: the GUI control channel has no matching
  command … use `aevum --mcp` to connect directly"
- `aevum_nope` → "unknown tool"

### Internal: three tool lists collapsed into one table

Tool IDs used to live in three places — the tool table in `aevum-mcp`, the command catalogue in
`aevum-ui`, and attach mode's description table in `aevum-mcp` — synchronised by hand. This round's
`xmp` defect grew out of exactly that lack of synchronisation. `mcp_routes` in `aevum-core` is now
the single source of truth, and the other three places are locked together item by item by tests:
add or remove one, misspell one letter, and local tests go red immediately.

Users need to do nothing: no tool name changed, just refresh `tools/list` once.

### Internal: the commit gate can actually stop people now

On a clean checkout, `cargo fmt --all -- --check` used to report 197 differences and clippy 4
warnings, so the first step of `scripts/check.sh` stopped right there — **the gate had long been
decorative**. The differences were not anyone's broken code: `rustfmt` rules had drifted with
`stable` because nothing pinned the version.

The repository now pins the toolchain to 1.98.0 in CI (same version as the dev machine), cleared
those 197 formatting differences and 4 clippy warnings, and all four steps (format → clippy
`-D warnings` → full tests → UI self-check) are green end to end. The repository root deliberately
had no `rust-toolchain.toml` at that point: downloads from rustup to the dev machine were unstable,
and a half-installed toolchain makes every `cargo` invocation stall in auto-repair, which would
leave the repository unbuildable; the file would be added once downloads stabilised, and CI would
be simplified along with it.

### Fix: saving only reported `os error 183` when `.aevum` was occupied by a file

If a frame folder happened to contain a **file** named `.aevum` (created by a slip of the hand, or
written by another tool), and there was no `.lumen` / `.lapse` directory to fall back to, you hit
this path:

- the sequence opened as usual, **with no notice at all**;
- saving reported `读写工程失败：文件已存在。(os error 183)`.

That string contains no path, no mention of "file", no mention of "directory" — users simply
concluded the program was broken. Now:

- opening the sequence prints a line to the terminal: "`.aevum` is a file, not a directory; the
  project will not be able to save";
- the save error became "there is a file of the same name at `D:\frames\.aevum`, not a directory,
  so the project cannot be saved; move it away or rename it and retry (the program did not touch
  it)" — the GUI's save-failure dialog shows this sentence directly.

The program never touches that file: no rename, no delete, no overwrite. Move it away and
everything works again.

### Fix: when the CLI is older than the GUI, extra commands no longer appear in the tool list

`--attach` takes its tool list from the GUI, while the "tool name → command" mapping lives in the
CLI itself. When the two versions differ and the GUI is newer, a tool appears that looks legitimate
but can never be called:

- before: it showed up in `tools/list`, the agent called it → "unknown tool". The agent would
  usually retry under another name, burning several rounds before giving up.
- now: it does not enter the list; at startup the CLI prints "GUI 0.6.0 reported 2 new commands
  this CLI does not know; skipped from tools/list … consider upgrading them together".

No information is lost: the total number of commands the GUI reports is still readable from
`system.commands`; it just no longer pretends it can call them. The reverse direction (GUI older
than CLI) was already safe and is unchanged.

### Fix: multi-scale denoise was grinding away too much detail

**Symptom**: pull the "multi-scale" slider up and the noise goes away, but so do the stars and the
texture. Measured against the atlas §46.2 criteria (noise variance down > 50%, MTF kept > 85%), the
old version preserved only 44–49% of the detail on real frame sizes — 27–40 points worse than
leaving multi-scale off.

**Two layers to the defect**:

1. **`eps` was decoupled from real noise.** The old formula
   `eps=(0.0001+0.006k)·max(1,(σ̂/0.006)²)` gives eps≈0.0344 at σ=0.02, which is 86× the noise
   variance. It is really a "contrast threshold", not a "noise variance": structures with variance
   below eps (sine amplitudes under √(2eps)≈0.26) were classified as noise and flattened, which is
   exactly why stars vanished. Below σ̂<0.006 there was still a fixed floor of `0.0001+0.006k`, so
   even the tiniest noise got ground.
2. **The degenerate frame size masked the problem.** The radii follow the frame size
   (`rc=short edge/130`, `rm=short edge/260`), and **below a short edge of 390 px both layers land
   on the floor of 2**, collapsing multi-scale into "the same 5×5 filter run twice". Early parameter
   sweeps were all done at 512×256 — what they measured was not the multi-scale structure at all.

**Fix**: `eps = 0.8·σ̂²/rm` (the 1e-6 floor only guards division by zero). `eps` returns to
variance units — the magnitude noise variance actually has; `1/rm` compensates for scale: the larger
the radius, the wider the neighbourhood a given smoothing coefficient is spread over, and the
stronger the effective smoothing, so the threshold has to come back down inversely with the radius.
The data forced this too: sweeping for the optimal gain separately, 1024×768 landed at 0.45 and
2048×1536 at 0.22, while 0.45×2≈0.9 and 0.22×5≈1.1 — the optimal gain is roughly inversely
proportional to `rm`, which is exactly this formula.

The parameter was swept, not guessed: C over {0.6, 0.8, 1.0, 1.2, 1.5} × {1024×768, 1536×1152,
2048×1536} exhaustively, and only 0.8 and 1.0 passed on all three frame sizes; 0.6 under-denoised at
1024×768 (49.1%), and 1.2 damaged detail at 1536×1152 (MTF 84.1%). 0.8 was chosen over the
dimensionally "rounder" 1.0 because 1.0 left only 0.6 points of MTF margin at 1536×1152 while 0.8
left 2.3.

**Before and after** (σ=0.020, nr_luma=50, multi-scale=100, variance↓ / MTF kept):

| Frame size | (rc,rm) | Single-scale (unchanged) | Multi-scale before | Multi-scale after |
|---|---|---|---|---|
| 512×256 (degenerate) | (2,2) | 69.5% / 84.5% | 72.0% / 64.2% | 26.7% / 93.6% |
| 1024×768 | (5,2) | 69.4% / 84.5% | 74.5% / 44.6% | **53.8% / 90.0%** |
| 1536×1152 | (8,4) | 73.5% / 76.6% | — | **62.7% / 87.3%** |
| 2048×1536 | (11,5) | 73.8% / 75.5% | 74.9% / 48.7% | **60.3% / 89.5%** |

**Measuring position**: all pass/fail readings are pinned to **1024×768** — the median short edge
of real photographs, and a size where rc=5 and rm=2 genuinely separate. The 512×256 row is kept for
contrast: it is still a degenerate frame (rc==rm==2), and 26.7% is not the formula failing — a 5×5
window simply cannot grind σ=0.02 noise on a 512-wide image. The old version "ground" out 72% by
inflating eps 86×, at the cost of leaving MTF at 64%.

**The cost, stated plainly**: at the same `nr_luma` setting, total denoise with multi-scale enabled
is **lower** than pure single-scale (1024×768: 53.8% vs 69.4%). What you get back is 5.5 points of
MTF: previously "half the detail for cleanliness", now "as much as the noise variance calls for".
Visually, multi-scale preserves more texture than before and leaves slightly more noise.

**What remains**:

- The single-scale branch was **not touched by a single byte** in this release; its 84.5% sits right
  against the 85% target, and only 75.5% at 2048×1536 — its own eps is unrelated to noise, and the
  `min/260` radius makes large images grind harder. That is a separate matter.
- The "multi-scale" slider direction is counter-intuitive: higher `nr_multiscale` grinds **less**.
  The cause is structural (`sm = mid-scale + ms·(coarse-scale − mid-scale)`, and the coarse scale is
  not filtered a second time); previously both layers' eps far exceeded the noise variance and the
  result saturated, hiding it. Now it is visible and deserves its own semantic fix.
- 3072×2304 (rm=8) was not measured: memory and runtime are not worth it, and `1/rm` there is
  extrapolation.

### Fix: the overshoot clamp on output sharpening never took effect

Export-time "sharpen for output purpose" (screen / glossy / matte × low / standard / high) contains
a clamp intended to suppress the white and black fringes on either side of an edge. In practice
**it never engaged once**: its threshold scaled **with** the strength (`0.12 × amount`), and that
threshold was 3× larger than the detail amplitude of a typical edge. The stronger the setting, the
faster the amplification and the threshold grew together — equivalent to no clamp at all.

Measured (low-contrast sine target plus step-edge target, per atlas §46.2):

| Strength | Overshoot before | Overshoot after |
|---|---|---|
| Standard (amount 1.0) | 8.35% | 2.09% |
| Highest (amount 1.4, matte + high) | — | 2.93% |

Under the old implementation overshoot grew proportionally with strength: amount from 1.0 to 2.0
took overshoot from 8.35% to 17.66% (the UI cannot reach 2.0; this only proves the clamp really was
inactive). Every setting is now within 3%.

The clamp now acts on the detail signal itself and is independent of strength, so overshoot stays
under 3% while the strength slider remains linear — low / standard / high can still be told apart on
the same edge. (A first attempt clamped the final output instead; all three settings collapsed to
the same value and the slider stopped working, so that approach was abandoned.)

The cost, stated plainly: at the same strength, edges are no longer pushed as hard, so sharpening
looks **slightly more conservative** than before. That is deliberate — the previous 17% overshoot
was not "sharper", it was a white fringe.

### Improvement: sharpening parameters now have reproducible objective acceptance

"MTF50 boost 1.2–1.6×, overshoot < 3%" used to exist only in design documents, guarded by no test
at all; the parameters could change and nobody would know. There is now a synthetic target set plus
metrics (sine target for MTF50, step target for overshoot) running directly in `cargo test`: change
sharpening defaults so that any setting falls outside the bounds and local tests go red on the spot.

The metrics self-check too — the measured amplitude of an ideal sine must equal the generated value,
an unprocessed step must have zero overshoot, and the pass-through response must be flat across the
band. If those three fail, every fitted number downstream is untrustworthy.

---

## v0.5.0-beta.1 — 2026-10-31

**Product rename: Lumen Studio / 光时 → Aevum Studio / 恒晷.**
This is the only breaking change in the release. **Not a line of algorithm changed**: every numeric
golden is byte-identical, and all eight UI self-check shape counts match 0.4 exactly.

### Rename

| Item | 0.4 | 0.5 |
|---|---|---|
| English name | Lumen Studio | **Aevum Studio** |
| Chinese name | 光时 | **恒晷** |
| Window title | 光时 · Lumen Studio | **恒晷 · Aevum Studio** |
| Executable | `lumen.exe` | **`aevum.exe`** |
| Green build directory | `dist/lumen/` | **`dist/aevum/`** |
| Launcher script | `光时.bat` | **`恒晷.bat`** |
| Crate names | `lumen-core` / `lumen-ui` / `lumen-mcp` | `aevum-core` / `aevum-ui` / `aevum-mcp` |
| Project directory | `<sequence>/.lumen/` | **`<sequence>/.aevum/`** |
| Config directory | `%APPDATA%/lumen-studio` | **`%APPDATA%/aevum-studio`** |
| Environment variables | `LUMEN_*` | **`AEVUM_*`** |
| MCP tool names | `lumen_*` | **`aevum_*`** |

### Upgrade notes (three sentences)

1. **Your old project's `.lumen/` is moved to `.aevum/` the first time you open that sequence.** If
   the move fails (read-only drive, antivirus holding the files) it **keeps working in the old
   directory** — no data loss, and it does not turn read-only. For a manual whole-library move, see
   `scripts/migrate-to-aevum.sh`.
2. **`LUMEN_*` environment variables still work**, but each process prints a one-line deprecation
   notice to stderr the first time it uses one. Prefer `AEVUM_*`; when both are set, `AEVUM_` wins.
3. **Refresh `tools/list` once in your MCP client after upgrading.** The server still accepts the
   old `lumen_*` names (silent aliases, byte-identical results), but **the list advertises only
   `aevum_*`**; some clients validate that "this name must appear in the list" before calling, and
   in that case the old name is rejected **by the client itself** — refreshing the list fixes it.

> **⚠️ Why old tool names get rejected**
>
> The server actually accepts `lumen_*`; what blocks it is **the client itself**: some clients check
> "this name must appear in my cached `tools/list`" before issuing `tools/call`, and the new list
> advertises only `aevum_*`. Refreshing `tools/list` once restores it — no configuration change
> needed.
>
> Also, the server name changed from `lumen-studio` to `aevum-studio`, so clients that remember
> authorisation by server name will ask once more. That is expected behaviour, not a defect.

### Compatibility

1. **Old projects open directly.** `.lumen/` is moved to `.aevum/`, and the older `.lapse/` moves
   the same way. Parameters, ratings, flags, keywords, keyframes, alignment results, deflicker
   curves, crop framing, camera motion and watermark settings are all restored as they were. When
   both old directories exist, only `.lumen` (the newer one) is used; `.lapse` is left untouched.
   Before moving, the program checks whether `.aevum/` is a **directory**: if a plain **file** of
   that name is already there, it does not overwrite it — it keeps using the old directory and says
   so in the terminal. Wiping a user's file while moving a directory is the one thing that must
   never happen.
2. **Old environment variables still take effect.** All 12 runtime variables (`LUMEN_LENSFUN` /
   `LUMEN_FFMPEG` / `LUMEN_LIBRARY` / `LUMEN_SETTINGS` / `LUMEN_PLUGINS` / `LUMEN_DCP` / `LUMEN_LCP`
   / `LUMEN_RENDER_QUEUE` / `LUMEN_FRAMES` / `LUMEN_DEV_CASES` / `LUMEN_REAL_RAW` / `LUMEN_ICC_DUMP`)
   support the legacy-name fallback, with the new name taking precedence and a one-line deprecation
   notice the first time the old one is hit. **The one exception is the packaging variable
   `AEVUM_FFMPEG_SRC`** — it exists only for `scripts/package.sh`, has no legacy fallback, and
   `LUMEN_FFMPEG_SRC` is no longer read from this version on. Portable-drive usage is unchanged:
   setting any override counts as portable mode, and `%APPDATA%` is never touched.
3. **The old config directory is merged in on first launch.** `%APPDATA%/lumen-studio` (or the older
   `lapse-studio`) becomes `%APPDATA%/aevum-studio`: if the new directory does not exist the whole
   directory is renamed (the old path disappears, but not one file is deleted); if it already
   exists, missing items are filled in one by one, the new directory wins on conflicts, and the old
   one is kept intact. The migration runs once.

   > The previous generation of the migration script moved only `render_queue.json`, which cost
   > users upgrading from the Python version their `settings.json` / `galleries.json` / `plugins/`.
   > This release moves at directory level and adds the `lapse-studio` fallback that had always been
   > missing on the Rust side. What was lost back then cannot be conjured back; what this release
   > does is "never lost again from here on".

4. **Old MCP tool names are still callable.** All 34 old `lumen_*` names still work, returning
   **byte-identical** results to `aevum_*`, with write-operation logging as usual. But `tools/list`
   advertises only the new names (see above).
5. **Launcher compatibility.** `恒晷.bat` search order: `dist\aevum\aevum.exe` →
   `dist\lumen\lumen.exe` → `target\release\aevum.exe` → `target\debug\aevum.exe`. Anyone who
   already unzipped the 0.4 green build changes nothing.
6. **Plugins are unaffected.** The `id` prefix in a plugin manifest is not parsed, and installed
   third-party plugins load as before. Only the in-repo sample plugin was renamed to
   `aevum.example-invert`; if you had both installed, two entries appear in the menu and you can
   delete the old one (user `plugin.toml` files are never rewritten automatically).

### Third-party components and licences

This project's own code is released under **MIT** (`LICENSE`). Third-party components shipped with
the binary carry their own licences; the full statement is in `THIRD-PARTY-NOTICES.txt` and
`licenses/`:

| Component | Licence | Relationship |
|---|---|---|
| `tools/ffmpeg.exe` (FFmpeg 7.1) | GPLv3 | Separate subprocess, not linked |
| **rawler 0.7.2** (RAW decode backend) | **LGPL-2.1** | **Statically linked into `aevum.exe`** |
| Inter / Noto Sans SC / Noto Emoji | OFL-1.1 | Compiled into the exe |
| Ubuntu Light / Hack / egui-icons / emoji-icon-font | UFL-1.0 / MIT | Compiled into the exe |
| Lucide icons | ISC | Compiled into the exe |
| The other 316 Rust crates | MIT / Apache-2.0 / BSD / Zlib family | Statically linked |

> **This section is a correction written on 2026-10-31.** Previously both `THIRD-PARTY-NOTICES.txt`
> and `docs/依赖许可说明.md` described rawler as "MIT OR Apache-2.0", and concluded from that
> "binary distribution carries no additional obligations". **That was wrong**: rawler is
> single-licensed LGPL-2.1 and is statically linked into the main program. This release fulfils
> three obligations under LGPL-2.1 §6 — the licence text
> (`licenses/LGPL-2.1-rawler.txt`), the corresponding source
> (`third-party-src/rawler-0.7.2.crate`), and concrete instructions for modifying and relinking it.
> It also closes the previously unmentioned gap around egui's default fonts (Ubuntu Light, Hack,
> Noto Emoji, egui-icons, emoji-icon-font).
>
> If your organisation forbids linking any copyleft code, build with
> `cargo build --release --no-default-features`: RAW decoding falls back to the built-in backend
> (behaviour identical to v0.3.0), the binary no longer contains LGPL/GPL code, and the cost is that
> a few formats such as CR3-CRX cannot be decoded.

### Known issues

- **The default ffmpeg path in `scripts/package.sh` is dead** (`../pythonProject4/.venv/...`, absent
  on most machines; **this predates the rename — not introduced by this release**). This release
  only improves the failure message: when it cannot find one it prints the probe order and the next
  command, then exits. Pass `AEVUM_FFMPEG_SRC=<absolute path to ffmpeg.exe>` explicitly.
- **The `lapse/` directory name is a historical leftover**: it is the Python reference
  implementation that mirrors the Rust pipeline formula-by-formula, unrelated to the product brand,
  and unchanged by the rename.
- **clippy / rustfmt were already red on the local toolchain** (4 lints plus 197 formatting
  differences, one of them inside a frozen layout file). This release guarantees **zero new
  differences** by design and does not force a "make everything green".
- **This release (`0.x`) does not promise cross-version compatibility**: throughout `0.x`, the
  project file format, presets, command-line arguments and the MCP tool set may keep changing, and
  every change is recorded in this file. (Historical note: from `1.0.0` on, the four compatibility
  contracts at the top of this file and in the README's "Versioning" section apply.)
- **A small number of tool names do not resolve in attach mode (`--mcp --attach`)**, and the `xmp`
  tools are named inconsistently between the two modes (`xmp_read`/`xmp_write` vs
  `xmp_import`/`xmp_export`). **This predates 0.4 — not introduced by this release**; the next round
  unifies the naming and adds exhaustive tests.
- **When a project lands on a read-only drive the UI gives no notice**: the guard directory cannot
  be moved and saving may fail, but only the command-line stderr carries a warning line — the GUI
  shows nothing. Fixing this needs UI warnings; tracked separately.
- **If a plain file named `.aevum` exists under the project directory, saving fails** (rare; usually
  created by mistake or by another tool). Opening works, parameters work, but **saving reports
  `os error 183`** ("cannot create a file when that file already exists"), and the message does not
  say what to do. This release **guarantees the file is not deleted** (migration skips it, keeps
  using the old directory and says so), but "no old directory, only that file" still cannot save.
  Workaround: rename or delete the file and retry. **This is a pre-existing 0.4 gap, not introduced
  by this release**; the next round adds a pre-flight check on guard-directory availability plus
  readable error text that tells the user where their data is.
- **When a portable-mode override path is invalid, the program silently falls back to the default
  config** with no diagnostic output. Tracked separately.

### Rollback

- **Program**: switch back to 0.4's `lumen.exe` / `dist/lumen/`.
- **Project**: the `project.json` format is identical in `.aevum/` and `.lumen/`; rename `.aevum/`
  back to `.lumen/` and 0.4 opens it.
- **Config**: rename `%APPDATA%/aevum-studio` back to `%APPDATA%/lumen-studio`.

> **⚠️ Do not open the same sequence alternately in the old and new versions.**
>
> Once 0.5 has moved `.lumen/` to `.aevum/`, if you open **the same sequence** again with a 0.4
> shortcut: 0.4 cannot find `.lumen/`, so it **creates an empty one**, and the UI shows default
> parameters — it looks like everything was lost, when in fact it all still sits in `.aevum/`. Edits
> you then make in 0.4 are written into that **empty directory**, and when you switch back to 0.5,
> 0.5 prefers `.aevum/`, so those edits are **silently ignored** and the two never reconcile.
>
> If you need to roll back, roll back once and for all: first rename the directory back to
> `.lumen/` (or the config back), confirm 0.4 behaves, then stay on 0.4. **Do not cross-open in
> between.**

### Legacy-name retention list (verifiable)

The following files still contain `lumen`, and **every occurrence is intentional**, not an
oversight. **6 categories, 29 files, 227 occurrences** (counted as `grep -oi lumen` **occurrences**,
case-insensitive — not lines, so the number is larger than the line count your editor shows).

`lapse/` and `oracle/` have **zero** `lumen` hits: the old name `lapse` exists only as a
**directory name** and is outside this `lumen` replacement. They appear in the exemption list below
because they are **wholly exempt directories**, not because anything remains inside — do not
confuse the two.

The `oracle/` gate deserves its own sentence: **`oracle/golden/` has zero diff (all 10 files
sha256-identical); `oracle/*.py` permits only comment / path-level changes, never code or numeric
changes.** The only change to `oracle/` in this release is one comment line in
`dehaze_downsample_ref.py` (replacing a **no-longer-existing old path** with the new one); not
changing it would have been the error, leaving a wrong path in the repository forever.

| Category | Files | Count | Why it stays |
|---|---|---|---|
| Compatibility-layer definitions | `crates/aevum-core/src/brand.rs`, `env.rs`, `config_dir.rs`, `project.rs`, `journal.rs` | 54 | The **single source of truth** for old directory names / old env prefixes / old tool-name prefixes. Delete any one and old projects, old env vars and old MCP tool names break instantly. The 19 old write-operation names in `journal.rs` matter most: dropping one does not error, it just quietly leaves gaps in the log |
| Compatibility tests | `crates/aevum-core/tests/project_roundtrip.rs`, `export_stills.rs`, `crates/aevum-mcp/tests/tools.rs`, `mcp_stdio.rs`, `crates/aevum-mcp/src/attach.rs`, `server.rs`, `lib.rs` | 59 | These `lumen` occurrences are the evidence that "the old spelling must still work"; deleting them deletes the tests |
| User-facing docs, migration tool and maintenance-script comments | `README.md`, `CHANGELOG.md`, `scripts/migrate-to-aevum.sh`, `scripts/package.sh`, `tools/fix_visibility.py`, `恒晷.bat`, `.cargo/config.toml` | 94 | Without the old name, users and the migration script would not know what to move; the `dist\lumen\lumen.exe` probe in `恒晷.bat` exists for people who already unzipped the 0.4 green build; the "the old name `LUMEN_FFMPEG_SRC` is no longer read" line in `package.sh` is an upgrade warning for whoever packages; the comment in `fix_visibility.py` explains why a hard-coded repo directory was replaced by a `__file__`-derived one — delete it and the next person to touch the script writes the path back in |
| Doc comments on the env-fallback promise | `crates/aevum-ui/src/settings.rs`, `crates/aevum-core/src/queue.rs`, `library.rs`, `lcp.rs`, `export.rs`, `dcp.rs`, `sequence.rs` | 8 | "(legacy name `LUMEN_*` fallback)" belongs exactly in the type documentation — it is a promise to callers |
| Brand-unrelated arbitrary strings | `crates/aevum-core/src/exif.rs` | 4 | The **fake vendor name** `"LUMEN"` in EXIF fixtures — the easiest arbitrary uppercase string in a test, unrelated to the brand; changing it only enlarges the diff |
| Frozen area / one-off historical scripts | `tools/split_step1.json`, `tools/split_step2.json` (plus `oracle/**`, `lapse/**` as wholly exempt directories, with zero `lumen` hits) | 8 | These two are **input snapshots** of UI-split scripts that have already run, with paths written as `lumen-ui` at the time; changing them would falsify the record. `oracle/` only had comment paths updated this round — zero change to code or goldens |

Two verification commands (Git Bash). The first must print `白名单外：0 个`:

```bash
allowed() { case "$1" in
  lapse/*|docs/*|oracle/*|tools/split_step1.json|tools/split_step2.json|tools/fix_visibility.py) return 0;;
  crates/aevum-core/src/brand.rs|crates/aevum-core/src/env.rs|crates/aevum-core/src/config_dir.rs) return 0;;
  crates/aevum-core/src/project.rs|crates/aevum-core/src/journal.rs|crates/aevum-core/src/sequence.rs) return 0;;
  crates/aevum-core/src/exif.rs|crates/aevum-core/src/queue.rs|crates/aevum-core/src/library.rs) return 0;;
  crates/aevum-core/src/lcp.rs|crates/aevum-core/src/dcp.rs|crates/aevum-core/src/export.rs) return 0;;
  crates/aevum-core/tests/project_roundtrip.rs|crates/aevum-core/tests/export_stills.rs) return 0;;
  crates/aevum-mcp/src/attach.rs|crates/aevum-mcp/src/server.rs|crates/aevum-mcp/src/lib.rs) return 0;;
  crates/aevum-mcp/tests/tools.rs|crates/aevum-mcp/tests/mcp_stdio.rs) return 0;;
  crates/aevum-ui/src/settings.rs|README.md|CHANGELOG.md|scripts/migrate-to-aevum.sh|scripts/package.sh) return 0;;
  恒晷.bat|.cargo/config.toml) return 0;;
  *) return 1;;
esac; }
bad=0
while IFS= read -r f; do
  grep -qi lumen "$f" 2>/dev/null && { allowed "$f" || { echo "白名单外命中：$f"; bad=$((bad+1)); }; }
done < <(git -c core.quotePath=false ls-files -co --exclude-standard)
echo "白名单外：$bad 个"
```

The second verifies the counts in the table above and must print `227 处 / 29 个文件`:

```bash
total=0; files=0
while IFS= read -r f; do
  n=$(grep -oi lumen "$f" 2>/dev/null | wc -l)
  [ "$n" -gt 0 ] && { total=$((total+n)); files=$((files+1)); }
done < <(git -c core.quotePath=false ls-files -co --exclude-standard)
echo "lumen 出现合计：$total 处，涉及 $files 个文件"
```

Use `git ls-files -co`, not `git grep`: `brand.rs` / `env.rs` / `config_dir.rs` / `.cargo/` are
**untracked** files, and `git grep` only searches the index — it would miss them and report a false
green. This has been hit once.

The third verifies `oracle/`. First the Python side (must produce **no output** — only comment-line
changes are permitted):

```bash
git diff -U0 oracle/ | grep -E '^[+-][^+-]' | grep -v '^[+-][[:space:]]*#'
```

Then the goldens themselves (must print `金标零 diff`):

```bash
git diff --stat -- oracle/golden/ | grep . && echo "金标被改了" || echo "金标零 diff"
```

The fourth verifies the old Chinese name (same counting method, `ls-files -co`, and must print
`光时 可疑文件：0 个`):

```bash
bad=0
while IFS= read -r f; do
  case "$f" in README.md|CHANGELOG.md|docs/*|lapse/*) continue;; esac   # the rename table must spell out the old name
  all=$(grep -o '光时' "$f" 2>/dev/null | wc -l)
  comp=$(grep -oE '曝光时间|测光时|吃光时|高光时|同光时' "$f" 2>/dev/null | wc -l)
  [ "$all" -eq "$comp" ] || { echo "可疑：$f（光时 $all 处，非品牌词 $comp 处）"; bad=$((bad+1)); }
done < <(git -c core.quotePath=false ls-files -co --exclude-standard)
echo "光时 可疑文件：$bad 个"
```

The criterion is not "does `光时` appear" but "do **all** occurrences of `光时` fall inside known
non-brand words". Chinese has no word boundaries — `曝光时间` / `测光时` / `吃光时` all contain
those two characters; searching only for "does it appear" flags legitimate Chinese words as
leftovers, while searching only for "are there leftovers" fails to prevent a replacement from
damaging them (7 of them were damaged this way in this round).

---

## v0.4.0-beta.1

The first beta after the Rust rewrite repository went public. Three lines of summary:

- **Lightroom-aligned UI round**: a multi-panel layout modelled on Lightroom, with full interaction
  for alignment and deflicker;
- **Algorithm milestone round**: alignment / deflicker / HDR / panorama / the main grading chain all
  implemented in Rust, with a formula-by-formula Python oracle and numeric goldens established;
- **Performance refactor round**: batching and caching across RAW decode, preview and the export
  chain.

See the repository history and mainline commits for details.
