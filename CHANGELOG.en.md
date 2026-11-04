# Changelog

[中文](CHANGELOG.md) · **English**

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

## v1.5.2 — 2026-11-04

Patch release: fixes a portrait photos "lying flat" orientation bug in preview and fusion.

### Portrait RAW: preview and fusion stand upright according to the capture orientation

Portrait photos carrying a rotation tag (EXIF orientation 5-8) used to lie flat on the
**canvas** -- preview, logical size, focus fusion and export all worked from the unrotated
sensor dimensions; only the filmstrip thumbnail was right. The cause was the RAW decode
backend (rawler 0.8.0) reporting "orientation 1" for every file, which upstream took to mean
every photo is upright. Orientation is now resolved from two sources: a non-1 backend value
wins (CR3 keeps its Exif inside the BMFF container, readable only by the backend), and a 1
(untrustworthy) falls back to the orientation tag read straight from the file header.

Affected: preview, sketch, focus fusion and export of portrait RAW files (RW2 / CR3 / NEF and
other bodies that write an orientation tag); landscape photos and embedded thumbnails are
unchanged. JPEG/TIFF were always correct and are unaffected.

Assertions: a new "every sample is upright on every decode route" test uses the orientation
code read independently from the file header as its baseline and reconciles it with the
backend's code; the negative mutation (removing the fallback) goes true red.

### Docs: README sample shots replaced with a new mountain-sunset sample

The five sample shots were re-taken with the fixed build, and one portrait sample now
demonstrates the upright result.

### Compatibility

- Project files, presets and parameters, command-line arguments, MCP tool set: identical to
  1.5.1; nothing added or removed.
- Upgrading from 1.5.1: replace the executable; no migration step. The fix only affects the
  display and fusion geometry of photos carrying an orientation tag; saved projects and
  parameters are unaffected.

## v1.5.1 — 2026-11-03

Patch release: fixes a bug in v1.5.0 that made headless focus fusion fail outright.

### `aevum_focus_merge`: adopting auto-alignment no longer reports "frame size mismatch"

In v1.5.0, headless `focus_merge` under the combination of `align: true` + adopted
alignment + no manual crop rewrote w/h to frame 0's **output** size inside the warp loop
and used it for the following frames: the output geometry was scrambled end to end and the
call failed outright with "the frames in the focus stack differ in size" -- headless focus
fusion was unusable under that combination (it fails loudly rather than silently producing
a bad image). The warp now keeps the source-frame size throughout and takes the output size
once after the loop, matching how the GUI side's identical orchestration was already
written. Affected combination: headless / MCP + `align: true` + multiple frames + no manual
crop; `align: false`, single-frame, and manual-crop paths are unaffected; the GUI path was
always correct.

Assertions: a new MCP end-to-end test pins "the adopted output tracks the reference frame"
-- this bug was its first catch; the negative mutations (reinstating the old loop / dropping
the block field / one uniform chain for every frame) all go true red. The same batch adds the
pyramid measure's window-semantics anchor, closing the ledger's block-field end-to-end and D1b
open items.

### Compatibility

- Project files, presets and parameters, command-line arguments, MCP tool set: identical to
  1.5.0; only the headless warp's geometric correctness changes, nothing added or removed.
- Upgrading from 1.5.0: replace the executable; no migration step.

## v1.5.0 — 2026-11-03

Feature release: focus-stacking batch two -- the **pyramid method** and
**align-before-fusion**. The MCP and command-surface contracts stay "add only, never change":
`focus_merge`'s `method` enum gains a third value and an optional `align` parameter; the
command catalogue is still 77 entries; the MCP tool set grows 35 -> 39 direct tools, adding
`aevum_stack_merge` / `aevum_timeblend_merge` / `aevum_cull_evaluate` / `aevum_cull_extract`.
The project file format is unchanged; the app settings file (settings.json) gains one defaulted
field that older files simply lack.

### Focus fusion: the pyramid method (a third method)

`method = "pyramid"`: multi-scale Burt–Adelson fusion on a Laplacian pyramid. Depth-map and
weighted-average blend the whole frame with **one** weight plane, so low frequencies (base
tone, broad gradients) follow that same weight and inverted halos appear just outside texture
boundaries. The pyramid splits the blend across scales -- fine levels decide only which frame
supplies a small detail, coarse levels decide only the base tone -- and neither leaks into the
other, cutting the main halo source. Levels follow the resolution automatically
(`clamp(floor(log2(h/32)), 3, 8)`), with the two coarsest levels weighted equally.

The cost, stated honestly: at 24 MP x 8 frames the pyramid measures **24.4 s / 4.43 GB peak**
(weighted average: 1.95 s / 2.88 GB) -- an order of magnitude slower and about 1.5x the peak,
in exchange for suppressing low-frequency halos. The UI default stays depth-map / weighted,
and the panel hover text spells out the trade-off. `feather` / `smooth_z` do not apply to the
pyramid: passing them passes validation but has no effect.

### Focus fusion: align before fusing

`focus_merge` gains an optional `align` (default `false`; omitting it is pixel-for-pixel
identical to passing `false`). When enabled, fusion first runs a focus-specific per-block
alignment: a block-wise shift field is estimated, each frame is warped per block, and only
then blended; if the gain is below threshold it reverts silently and blends the raw frames.
The panel gains an "align before fusing" toggle with a three-state status line, and that
preference **persists** to `merge.focus_align` in settings.json. Known-limitation items 2
(two methods only), 5 (no built-in alignment) and 6 (merge parameters not persisted) from 1.4.0
are thereby closed.

### Headless control plane: stacking and time-blending enter MCP (tools 36-39)

Abilities the GUI has long had get headless entries: `aevum_stack_merge` (star-field stacking:
trails / noise reduction / satellite removal / sigma clipping), `aevum_timeblend_merge`
(time blending: horizontal bands / horizon split), `aevum_cull_evaluate` (culling evaluation:
pick keepers by metering; headless meters into session memory and never reads the on-disk
cache, divergence recorded as C7 in the ledger), and `aevum_cull_extract` (extract keepers
into a new subsequence: an explicit `exclude` index list replaces the GUI's session flags,
usually fed from `aevum_cull_evaluate`'s `indices`). Framing convention matches
`aevum_focus_merge`: a headless session has no "current frame", so an omitted range starts
from the middle frame.

### Measured performance and one optimisation round

The pyramid first measured 29.7 s; after one optimisation round (measure computed once,
streaming reduce, copy-free weight planes) it measures **24.4 s** with the peak unchanged --
the bulk is the tower's own memory-bandwidth work. QA's independent re-run measured 26.4 s:
absolute values jitter with machine and build configuration (within ±20%), so judge by the
same-session ratio. Measurement protocol and the before/after comparison are in
`docs/焦点合成-调研.en.md`, section 5.

### Known limitations

1. Focus fusion still takes at most **32 frames**; the pyramid uses more memory -- 8 frames at
   24 MP measured a 2.10 GB fusion delta (weighted: 732 MB).
2. GPU (OpenCL) remains unimplemented; the panel still offers no depth-map / assignment preview.
3. Headless sessions still have no "current frame": omitting `start` still starts from the
   median frame.
4. Two assertion gaps stay honestly open (`docs/遗留台账.en.md`): an end-to-end assertion on
   the per-block shift field, and an assertion anchor for the pyramid measure's box-average
   window semantics.

### Compatibility

- Project files, presets and parameters, command-line arguments: identical to 1.4.0; old
  projects open as-is.
- MCP tool set: `focus_merge`'s `method` enum gains `pyramid` (additive); new optional `align`
  (default `false`, not in `required`); four new headless tools (`aevum_stack_merge` /
  `aevum_timeblend_merge` / `aevum_cull_evaluate` / `aevum_cull_extract`), 39 in total.
- settings.json gains `merge.focus_align` (serde default `false`): 1.4 settings files lack the
  field, read back as `false`, behaviour identical to 1.4.

## v1.4.0 — 2026-11-02

The pre-release audit pass: closing the gaps focus stacking left behind -- a headless
`aevum_focus_merge`, the `focus.merge` framing parameters brought in line with the
implementation, an "align first" prompt in the panel -- plus four defects that could kill a
long job halfway or eat all the memory. The MCP tool set gains one entry (34 -> 35 direct
tools); **the command catalogue is still 77** (the "72" in the README was a stale number,
corrected here). The project format, presets and parameters, and command line contracts are
unchanged.

### Headless: `aevum_focus_merge` (direct tool no. 35)

The GUI had focus stacking; headless had only HDR / panorama / star stacking / time
blending -- an agent wanting a focus bracket had to assemble frames and go through export.
It now follows `aevum_pano_merge`'s shape: eight frames from the sequence's middle frame by
default, or an explicit `frames` list; alignment offsets and the uniform crop apply when an
alignment report exists, mismatched sizes are refused; errors speak of "focus stacking", not
"star stacking"; the still lands as `focus_{first:06}_{N}f.{ext}`.

### `focus.merge`: framing parameters match the implementation

The contract table said `center` while the implementation read `start` -- passing `center`
per the 1.3.0 schema never did anything. 1.4.0 renames the contract to `start` (the
implementation is untouched): `start?` is the first frame index, `around?` is how many more
frames to take after it (1..31, 7 by default, eight frames in all).

### Focus stacking: the panel says "align first"

A handheld focus pull usually drifts a few pixels; fusing without aligning ghosts along the
transition band, and ghosting looks exactly like "the depth map picked the wrong frame". The
focus group now says so up front and offers an "Align the sequence first" button right
there. All four merge groups share one sentence for the framing row, and the caption spells
out the real frame count (N either side -> 2N+1 frames; N forward -> N+1 frames, cap 32).
`focus.merge` also returns `aligned`, saying whether this run applied the alignment offsets.

### Real bugs fixed

1. **The metering cache's replace window**: `measures::save` deleted the old file before
   renaming the new one, leaving an instant where the cache file did not exist; another
   extraction reading at that moment got "the system cannot find the file specified". It now
   renames straight over the target (Windows `fs::rename` goes through
   `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`, measured to overwrite) and retries once after
   10 ms on failure.
2. **Cache contention during cull extraction**: when another process moved the source
   sequence's metering cache away, the whole extraction failed. It now retries `NotFound`
   three times, then skips the cache and extracts anyway; other IO errors still surface.
3. **Focus stacking's memory and integer overflow**: the ownership map no longer computes a
   whole row of measure planes before comparing -- it compares as it goes, an order of
   magnitude less peak memory. An enormous `feather` no longer panics on integer overflow.
4. **Symlink and junction traversal on the output directory**: when `out_dir` was a junction
   pointing back to the sequence root's drive, the old lexical containment check saw nothing
   wrong and the still was written somewhere the user never named. The check now resolves the
   deepest existing ancestor first and refuses when lexical and real containment disagree.

### Known limitations

1. Focus stacking takes at most **32 frames**: every frame is resident, estimated as
   `frames x pixels x 12 B + three output planes`, which is about **9.6 GB** for 32 frames at
   24MP. Deeper stacks fuse in passes.
2. Only the **depth-map** and **weighted-average** methods; pyramid fusion (survey item P2)
   and GPU (OpenCL) are not implemented in this round.
3. The panel has no depth-map preview: per-pixel ownership is not visible, only the fused
   result.
4. A headless session has no "current frame": `aevum_focus_merge` starts from the sequence's
   **middle frame** when `start` is omitted; pass `start` or `frames` to pick another span.
5. **There is no built-in auto-alignment**: run "Align sequence" before fusing. The focus
   group says so and offers an "Align the sequence first" button right there; skipping it
   leaves ghosting across the transition band (which looks like "the wrong frame was
   picked").
6. **Merge parameters are not persisted**: focus stacking's `method` / `radius` /
   `threshold` / `smooth_z` / `feather` live only in the current session and fall back to
   their defaults after a restart or a sequence switch. Star stacking and time blending
   behave the same way.

### Compatibility

- Project file, presets and parameters, command line arguments: identical to 1.3.0; old
  projects open as-is.
- MCP tool set: adds `aevum_focus_merge` (direct tool no. 35); the existing 34 are unchanged.
- **One contract correction**: `focus.merge`'s `center` never did anything (the
  implementation reads `start`), so 1.4.0 renames it to `start`. A script that passed
  `center` per the 1.3.0 schema had that field ignored all along -- pass `start` instead and
  the behaviour matches what 1.3.0 actually did.

## v1.3.0 — 2026-11-02

Feature release: ledger gaps B6-B9 closed in one pass — three time-blending features
(deghosting, automatic horizon, half-resolution preview) plus "extract a subsequence" in
the cull station. The MCP tool set gains one entry (`cull.extract`, 76 in total); the
project format, presets and parameters, and command line contracts are unchanged.

### Time blending: deghosting (B6)

Moving subjects — clouds, trees, traffic — no longer smear across band boundaries. The ACR
chain: split each frame into 16 px blocks and take means; a block deviating from its own
3x3 **spatial** median by more than the threshold (EV) is labelled as motion; the label map
is box-filter softened while taking the max against the original (keeping full rejection);
blending decays weights by label; a fully rejected tile falls back to the highest-weight
source frame. Static content (vignette, horizon) is labelled on both frames and returns
untouched. The threshold is the panel's "deghost" slider (EV, 0 = off).

### Time blending: automatic horizon (B7)

No more hand-placed horizon for sky-ground composites: all frames are averaged into a
per-row brightness profile, smoothed, and the strongest vertical step within the 10%-90%
window wins — provided the sky-ground contrast is at least 0.5 EV (flat light or pure sky
falls back to the manual value).

**This feature flushed out a real bug**: the sky-ground weight numerator was inverted,
laying the sky across the bottom half. Old tests using identical frames could not see it; a
stronger fixture (ground `lin(0)` / sky `lin(2)`) exposed it immediately; fixed in
`8b18dd1`.

### Time blending: half-resolution preview (B8)

See the composite in the panel without writing a file first: whenever a parameter or the
frame selection changes, a half-resolution composite is recomputed in the background (2x2
box filter, with a decode cache — unchanged frames only redo the composite). Late stale
replies only refresh the cache, never the on-screen texture. The preview runs the same
alignment and crop path as the final still, and the acceptance test requires the preview to
match the halved real still point by point.

### Cull station: extract a subsequence (B9)

`cull.extract` (MCP command no. 76): copies the kept frames into a new sequence folder with
three path-safety refusals (inside the source / a drive root / a non-empty target), mtime
preservation, the metering cache (`.aevum/measures.json`) carried over whole, and a
`dry_run` estimate that never touches the disk. The panel gains a folder input and
"Estimate / Export" buttons.

### Compatibility

- Project format, presets and parameters, command line: identical to 1.2.1; old projects
  open as-is.
- MCP tool set: `cull.extract` added (no. 76); the existing 75 are untouched — additive
  only, so older clients are unaffected.
- The new time-blend parameters (`horizon_auto`, `deghost`) carry defaults: omit them and
  you get the new behaviour (auto horizon off, deghost 0.5 EV). The `bands` validation is
  narrowed to the time-slice layout, so sky-ground is no longer blocked by the panel's
  default (itself a real defect fix).

---

## v1.2.1 — 2026-11-02

A patch release in name, but a substantial one: four **real bugs** fixed (two of which made RAW
import and HDR export look outright broken), plus a batch of tone and UI work. **No external
contract changes** — the four promises (project format, presets and parameters, command line,
MCP tools) hold exactly as in 1.2.0.

### Real bugs fixed

**1. Every spec-compliant DNG was rejected as "CFA pattern unusable"**

In DNG (TIFF-EP / DNG 1.6 §4.6.2 Table 8) `CFAPattern` is **0-based**: `0=R, 1=G, 2=B`. The code
read `2/3/4`, off by one, so every DNG written to spec failed with "CFA pattern is not a usable
2×2 RGBG layout". Synthetic targets could not catch this — target and implementation shared the
same misreading and were wrong in the same place. A real iPhone DNG exposed it immediately.

**2. The iPhone CFA image lives in SubIFDs, not IFD0**

In that same DNG, IFD0 is an 852×640 YCbCr preview; the 4032×3024 raw data sits under `SubIFDs`.
Looking only at IFD0 means always getting the preview. The loader now collects IFD0 + SubIFDs,
excludes thumbnails via bit 0 of `NewSubfileType`, and takes the largest image.

**3. HDR export flattened everything above white**

Three clamps plus the curve tail pinned every value above 1.0 back to white. `Opts::hdr` now
routes those clamps and the tone curve keeps its tail (`lut::tone_curve_keep_over`). The SDR path
is bit-identical (golden md5 unchanged).

**4. Twelve export tests never ran in CI**

Seven in `export_video`, three in `queue_export`, one in `retime_export`, one in `export_stills` —
all start with `let Some(ffmpeg) = ffmpeg() else { return };`. No ffmpeg, no work, and the test
still reported "ok"; and CI explicitly did not install ffmpeg. Worse, those files pinned ffmpeg to
a **hard-coded absolute path** and did not even consult `AEVUM_FFMPEG` — move the repo and they go
silent forever.

The probe now uses the production `find_ffmpeg()` (`AEVUM_FFMPEG` → `tools/` next to the exe →
`PATH`); **no ffmpeg means red**; CI installs ffmpeg. To skip deliberately you must set
`AEVUM_ALLOW_NO_FFMPEG=1`, and it prints a line saying the test verified nothing.

### Added

- **HDR video**: x265 `main10` + PQ VUI (`bt2020` / `smpte2084` / `bt2020nc`). HDR video is only
  allowed as H.265 + Rec.2020; other combinations are refused rather than silently downgraded.
- **HDR inspection in the preview**: "is there anything above white?" — a soft fold at 0.90 that
  leaves everything below white bit-identical.
- **Tone**: dehaze constants fitted against known-truth haze targets; HDR curve tail re-derived
  from measurement; `contrast_linear` pivot fixed (mid grey stays at 0.5049 across the range);
  the `masking` dead zone is gone (0–50 now responds across the whole slider).
- **Output sharpening**: the three presets and manual σ mapping are complete.
- **UI**: the three preset sections can be dragged into a new order and persist; the three
  floating windows remember their position across launches (clamped back into the viewport).

### Interface and experience

- **Error messages follow the UI language.** Errors used to be produced as finished Chinese
  sentences, so an English UI still showed Chinese errors, sometimes half-English half-Chinese.
  A message is now split into "template + arguments": the template is a translatable key, the
  arguments are values (numbers, paths), and nested errors are translated recursively. Logs
  (`Display`) stay Chinese; the UI renders in the current language.
- **The language list is registry-driven**: adding a language means one table plus one line in
  `CATALOGS` — no third place.
- **New icon**: an A whose blue crossbar is the shadow a gnomon casts — the letter for Aevum and
  the meaning of 晷 in one mark.

### Decided against

- **16-bit HDR in TIFF/PNG.** Declaring PQ there is only possible through an ICC `curveType`
  sampled at 4096 points; Photoshop, Windows Photos and most viewers do not parse it and show a
  grey wash. Shipping linear 16-bit instead is interpreted as gamma by those same tools — a more
  insidious error. So: **refuse and explain**, with the panel snapping to AVIF + Rec.2020.

### Known issues

- **HDR applies to AVIF stills and H.265 video only.** 16-bit HDR in TIFF/PNG is out (see above).
- **There is no HDR preview mapping.** The panel shows whether anything exists above white, but
  that is not a simulation of a 203-nit HDR display — panel brightness is not something software
  can change, and faking it would only impose a highlight roll-off on the user.
- **45° edges are bounded by the sampling pattern.** At R/B sites the green neighbours are only
  up/down/left/right; on a 45° line the neighbours are same-colour R/B, not G, so the boundary
  ring is off by a full step. Measured error 2190 ≈ 0.168 × step, matching the 1/6 boundary share;
  away from the boundary nothing is off at all. Other methods (cell average 2167, directional
  2190) land on the same line.
- **There is exactly one real camera RAW sample** (an iPhone 6s Plus DNG, CC0). CFA mapping and
  IFD selection are pinned against a real file; other bodies (CR3 / X-Trans in particular) remain
  unverified.

---

## v1.2.0 — 2026-11-01

Feature release. **One thing up front: this version starts changing pictures in existing
projects.** Two of the fixes correct behaviour that was wrong — half of the `masking` range was
inert, and `contrast_linear` pivoted a stop and a half off. If you need the old look back, the
roll-back is at the end.

All four public contracts (project format / presets and parameters / command line / MCP tool set)
are add-only: a new `dynamic` field and argument, which old projects read as SDR with
byte-identical output.

### HDR export: AVIF 10bit + PQ

"HDR export (PQ / AVIF 10-bit)" used to be a reserved entry point with a `todo!()` behind it. It
now writes real files.

Choosing **HDR (PQ)** exports through the PQ transfer function (SMPTE ST 2084), BT.2020 wide
gamut, and 10bit coding, in an AVIF container. Three decisions are pinned, each with its own
criterion:

- **Reference white 203 cd/m²** (BT.2408 HDR reference white). The panel's "white" lands on 203
  nits in an HDR file, not 100 — mapped to 100, the same picture reads one stop darker than the
  SDR window next to it on an HDR display.
- **No highlight compression.** Anything above white keeps climbing along the curve tail, all the
  way to PQ's 10000-nit ceiling. Which means the "HDR tail" slider finally has a destination in
  HDR export.
- **Full-range coding**, not video's 16–235.

Only AVIF can hold HDR, and it must pair with Rec.2020: picking HDR switches the colour space and
container over by itself (the selections move — you can see it), and exporting any other
combination is refused with a reason instead of quietly falling back to SDR. A quiet fallback is
the worst case here: the interface says HDR, the file is SDR, and it looks flat grey on an HDR
display.

Final renders (video) still come out SDR only, so the switch is not shown in video mode.

### Fixed: half of the `masking` range was inert

The sharpening "masking" slider did **nothing at all** to flat areas anywhere from 0 to 50:
measured noise gain at 0 / 10 / 25 / 50 was 1.944 across the board, identical to no masking. The
slider moved and not one pixel changed; it only started working past 75.

The threshold was a squared term, which flattened the low end. Switching to a square root makes it
respond from 25 on: 25 → 1.816, 50 → 1.385, 75 → 1.174. Both ends are unchanged, so `masking=100`
renders exactly as before.

**What changes:** existing projects with masking between 25 and 100. The RAW import default is 20,
which sits in that range — those pictures will show slightly cleaner flat areas.

### Fixed: contrast (linear) no longer darkens mid-gray on the side

`contrast_linear` means "compute the contrast increment in linear light", but its pivot sat at
linear 0.5, which is encoded 0.735 (73% gray). Fully turned up, mid-gray 0.5 was pushed down to
0.4487 — 14 levels (8-bit) gone for free. The pivot now sits on mid-gray: mid-gray does not move at
any strength, and shadows and highlights pull apart in opposite directions.

One uncomfortable conclusion that comes with it: **with the pivot fixed, this slider's visible
effect is down to about 1 level** (max 0.0041 measured). Almost all of its former "effect" came
from the misplaced pivot, not from linear light. So do not expect much from pulling it — its value
is in computing contrast in linear light at all, not in how far it moves.

**What changes:** existing projects where contrast is non-zero and contrast_linear is non-zero.

### Fixed: AVIF export never actually worked with a real ffmpeg

Stills are written to a `<name>.part` temporary file and renamed atomically; the extension is
`part`, so ffmpeg cannot infer the container and reports `Unable to choose an output format`. Both
paths now pass the container explicitly.

The bug has been there since AVIF shipped and went unnoticed because every AVIF test is "skip when
no ffmpeg is found" — neither this machine nor CI had one, so it stayed green all along.

### Known issues

- **HDR applies to stills + AVIF only.** Video (x264/x265) is still 8bit SDR, and 16bit HDR in
  TIFF/PNG is not done either — that needs a PQ curve inside the ICC profile, not a one-liner. Any
  combination that cannot hold it is refused.
- How an HDR file looks on an SDR screen is up to the player's tone mapping; there is no built-in
  SDR preview mapping. The panel keeps showing the SDR preview — check HDR on an HDR display or in
  a player.
- Directional demosaicing is still well below ideal on 45° edges and coloured boundaries: the
  direction test only considers horizontal and vertical, and diagonal detail falls back to the
  colour-difference constancy assumption.
- There are no real camera RAW samples on this machine, so that demosaicing data all comes from
  synthetic targets; behaviour on real photographs remains unverified.

### Roll-back

Install `v1.1.0`. Project files, presets and `settings.json` are forward compatible — this version
only adds the `render.dynamic` field, which the older version ignores. What you cannot get back is
the rendering: `1.1.0` keeps the old masking mapping and the old pivot.

To keep the old look without downgrading: set masking to 100 (the two ends agree there) and set
`contrast_linear` to zero.

---

## v1.1.0 — 2026-11-01

A feature release. The four public contracts (project format / presets and parameters / command
line / MCP tool set) are untouched; install over the previous version. The version moves from
`1.0.1` to `1.1.0` because this release **changes what you see when you open a photo**: the right
panel no longer unfolds all eighteen groups at once.

### The right panel opens with only the everyday groups expanded

The Develop module used to open with all eighteen groups unfolded, so reaching "Basic" meant
scrolling a screen. A registry of 53 groups now decides what starts expanded: the groups you touch
on every photo and every export open; the ones you set once and forget (camera and lens setup,
watermark, HDR and panorama merge) start collapsed — the same convention as Lightroom, where only
"Basic" is open when an image loads. The Develop module's shape count on first paint drops from
2193 to 1144, roughly half the drawing.

The collapse state is remembered: whatever you fold stays folded on the next launch. Right-click a
group header for "show only this group / collapse all / expand all".

**This is the only change in this release that may make you go "huh, that's different".** To get
the old look back: right-click → expand all, once.

### Sub-windows can be dragged, and the corners finally match

The three floating windows (plugins, presets, appearance) refused to move when dragged. They used
egui's `anchor`, and the framework docs say it plainly: anchoring also makes the window immovable.
They now use a default position for the first placement and remember where you drag them.

Two details came along: the title bar used to square off the window's top two corners, and all four
now follow one scale; full-width rules inside windows became indented soft separators, so the
window no longer reads as "cut in half".

### The bottom filmstrip collapses with one click

The "film" toggle in the timeline header gained a rotating arrow (collapse / expand) and a tooltip.
The header row stays when collapsed, so the way back is always on screen.

### Appearance and language now have an entry point in the title bar

The customisation feature — appearance, light or dark, skin, accent, corner radius, density, type
size, layout, filmstrip — already existed. What was missing was a way in: it lives inside the
"Appearance & Layout" window, and that window had no permanent entry. A gear now sits to the left
of the export button; `Ctrl+,` still works.

### Crop: nine preset ratios

Original / 16:9 / 3:2 / 4:3 / 5:4 / 1:1 / 4:5 / 9:16 / 2:3, laid out the way Photoshop does.
MCP's `crop.set` accepts the same nine.

### History: one drag is one step

Dragging a slider used to leave a dozen entries — the intermediate states plus the echo when you
let go — so finding "the step I just made" was hard. Consecutive operations of the same kind now
merge into one.

### English UI: the remaining Chinese strings are gone

Untranslated hints, status-bar text, command errors and tooltips are filled in; the entry count
goes from 706 to 1103. This release also turns "a string is untranslated" into a static check that
fails on commit, so they will not quietly creep back.

### A directional demosaic option

The diagnostics panel now offers "directional" as a demosaic method (Hamilton–Adams edge-directed
green interpolation plus a constant-chroma assumption for red and blue).

Against four synthetic scenes with known ground truth (vertical stripes, a 45-degree edge, fine
texture, a colour edge) its overall PSNR is 3.0–10.6 dB above the old 2x2 cell average. The cost
is full-resolution decode time: 27 ms to 256 ms at 24 megapixels.

**The default is unchanged — cell average.** If you do not pick it, nothing moves. Why the default
was not switched: the DNG path carries its own copy of the cell-average demosaic, and both paths
have to change together, otherwise the same lens produces a DNG and a camera RAW with two different
levels of sharpness. The DNG path is pinned by per-pixel golden data, so changing it means
refreezing those. That waits for the next release.

### Known issues

- **`masking` does nothing between 0 and 50.** This is a pre-existing problem found while measuring
  this release: dragging the slider to 25 or 40 moves the control but has no effect on flat noisy
  areas at all (the threshold is a square term, so the low end is nearly flat), and it only starts
  working past 75. Fixing it means changing the mapping curve, which would make **existing projects
  with masking between 25 and 50 change appearance**, so it waits for a release where that call can
  be made deliberately. Until then: push it past 75, or rely on sharpening protection (which RAW
  imports enable at 20).
- **HDR export (PQ / AVIF 10-bit) is still a placeholder, not a working feature.** `write_avif`
  currently goes through ffmpeg as 8-bit.
- The directional demosaic is still well short of ideal on 45-degree edges and colour boundaries:
  its direction test only considers horizontal and vertical, so diagonal detail falls back to the
  constant-chroma assumption.
- There are no real camera RAW samples on the development machine, so every demosaic number above
  comes from synthetic scenes; behaviour on real photographs is unverified.

### Rolling back

Install `v1.0.1` over it. Project files, presets and `settings.json` are forward compatible — this
release only adds a `collapsed` field, which older versions ignore. What does not come back is the
look: `1.0.1` has no collapse memory.

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
