# RAW compatibility: root cause and defences

[中文](RAW兼容性-根因与防线.md) · **English**

> Nature: an incident post-mortem plus a description of the defences. Not release notes and
> not a to-do list -- the unfinished parts live in [`遗留台账.en.md`](遗留台账.en.md) item C8.
> Conclusion first: **this is not "one format is unsupported", it is "a third-party parser was
> treated as trusted code".**

## 1. What happened

On 2026-11-02 the user supplied ten real camera samples (`E:/test-p`, 164 MB), they were
copied into `samples/cameras/`, and the very first run caught this:

```
thread '<unnamed>' panicked at rawler-0.7.2/src/bits.rs:246:
index out of bounds: the len is 1024 but the index is 1024
```

The scene was an iPhone `IMG_1381.DNG` (LinearRaw). The panic line is not what matters; what
matters is that **nothing followed it**:

- the panic happened inside the rayon workers rawler starts itself, and was swallowed by the
  library's internal `scope`;
- the main flow returned `Ok` as usual and handed back a **partially un-linearised** image;
- the interface looked perfectly fine -- curves responded, export produced files; only the
  shadow data was wrong.

A crash gets reported as a bug; silently dirty data never does. The cost of this class of
error has to be counted in "the batch of renders the user believed were correct".

The same path on `rawler 0.8.0` (measured this round, same machine, same file):

```
thread '<unnamed>' panicked at rawler-0.8.0/src/bits.rs:248:
index out of bounds: the len is 1024 but the index is 1024
```

The panic still happens (the panic hook still writes to stderr), but 0.8.0 wraps it in its own
`catch_unwind` and returns `Failed to decode image, possibly corrupt image: Caught a panic
while decoding.` Our routing therefore **falls back to the built-in path**, decodes the
camera-embedded 4032×3024 JPEG preview, and labels it honestly as `RawPath::Preview` -- no
crash, no dirty data, no pretending it is sensor data.

## 2. Root cause (four layers; dropping any one makes the incident impossible)

### ① A third-party parser treated as trusted code

Rust's `Result` only covers the paths the author **thought of and returned an error on**. Paths
nobody thought of -- the `bits.rs` style of "compute an index from the bit width and index the
buffer directly" -- panic when out of bounds. This is not poor implementation quality in
rawler; it is the norm for any heavily optimised binary parser, `image` and `png` included.
**The trust boundary is not "is it written in Rust", it is "who gave us these bytes".**

### ② `=0.7.2` pinned exactly, which pinned the upstream panic fix too

`Cargo.toml` said `rawler = "=0.7.2"`, and the reason was legitimate: 0.8 requires rustc 1.89,
above the MSRV of 1.88 declared at the time. The price was that upstream's guards against this
class of panic, and its model-table updates, could not be taken. **Pinning is a virtue for
reproducibility and a vice when eating untrusted input**: fixes in this kind of library are
usually "one more body / one more compression no longer panics", not new features.

### ③ The panic happened on a worker thread → silently dirty data

This is the only layer **unique** to this incident. A main-thread panic is a crash, which is
visible; a worker-thread panic swallowed by `scope` means nobody fills that slot and the caller
receives an artefact with a piece missing. Every "parallel + per-frame independent failure"
architecture has this surface: **the behaviour after one slot fails must be decided
explicitly**, otherwise the default answer is "a value that looks normal".

### ④ One CC0 sample, no vendor matrix

`samples/cc0/` held a single iPhone 6s Plus DNG. The assertion could prove "this one body
decodes" and nothing about a second body, a second vendor or a second compression scheme.
Worse, it **shaped the fixtures**: a synthetic fixture and the implementation are written by
the same person from the same understanding, so a shared mistake keeps both green (that is how
the CFA pattern mapping survived until 2026-11-01: every **conforming** DNG was rejected and
only the in-house fixture passed). Real samples are the only thing that breaks that loop.

## 3. Defences (four, narrowing at each step)

### 3.1 Dependency version: 0.8.0, with the reason written next to it

`rawler = "=0.8.0"`, and the MSRV moves up to 1.89 with it (the interface code only uses the
1.88 let-chain, so 1.89 takes nothing away and the bump costs nothing). The `Cargo.toml`
comment says plainly that we **must stay on 0.8+** and why -- the next person tempted to save a
compile will read it first.

### 3.2 One `decode_guard`: every entry point for untrusted bytes goes through it

`crates/aevum-core/src/raw/guard.rs`, two functions:

| Function | Used at | Why this shape |
|---|---|---|
| `decode_guard(what, f)` | boundaries whose error vocabulary is already `RawError` (rawler's `decode_file`, the container's embedded JPEG decode) | the closure returns `Result<T, RawError>`, so a panic becomes `RawError::DecoderPanic` directly |
| `catch_decoder_panic(what, f)` | boundaries with a different error vocabulary (plain-image decode in `metrics`, thumbnail cache read, export size probe) | it preserves the caller's own error type: a plain JPEG must not be reported as "RAW decode failed". `decode_guard` is its specialization |

Covered boundaries (five, each with a comment saying where the bytes come from and why they are
untrusted):

1. `raw/rawler_backend.rs`: `rawler::decode_file` (third party, and it parallelises internally)
2. `raw/preview.rs`: the **size probe** (`into_dimensions`) and the **decode** of the
   container's embedded JPEG
3. `metrics.rs`: plain-image `reader.decode()` (jpge / zune are third party as well)
4. `metrics.rs`: the thumbnail cache read (the file has lived on the user's disk and may be
   truncated or replaced)
5. `export.rs`: the size probe before export (headers only, but one bad file must not panic an
   entire export)

**Plus one outermost layer**: the whole body of `metrics::decode_with_origin` is wrapped. The
reason is that it is the **single funnel** from a path to pixels (sequence decode, metering,
thumbnails and export all pass through it), and the funnel holds not only third-party decoders
but **this repository's own** TIFF/DNG parsers, which eat untrusted bytes just the same and can
panic on input nobody anticipated. Precise attribution still lives in each branch's own guard;
the outermost layer only guarantees that "the consequence of a panic is this frame being
skipped, not the whole sequence disappearing".

The `UnwindSafe` boundary is spelled out in the module header: everything here is a
**read-only decode**, the decoder is consumed inside the closure, and no half-built state is
reused after a panic. The few adapters holding a `&mut` (`ImageReader` is moved into the
closure) acknowledge it with `AssertUnwindSafe`, for the same reason.

### 3.3 Error classification: if it has a name, the user knows what to do next

| Situation | Error | What the user reads | Next step |
|---|---|---|---|
| Suffix recognized, no decoder in this version (HEIC/HEIF + 15 vendor RAW suffixes) | `RawError::UnsupportedFormat { ext }` | "`heic` is not supported yet; dropping a sample into `samples/cameras/` lets a later release add it" | convert the file, or send us the sample |
| Decoder panicked internally (caught by the guard) | `RawError::DecoderPanic { backend, detail }` | "The rawler decoder hit an internal error; skipping this frame (your file is not corrupt)" | skip this frame, carry on |
| Container/compression not decodable | `NotTiff` / `Rawler` / `Image` ... | each with its own specific structural description | fall back to the preview, or skip |
| Layout not understood (capability boundary) | whichever `soft_fallback()` returns true for | -- | **fall back to the embedded preview**, labelled honestly via `RawPath` |

Two suffix tables, two different questions; never merge them: `RAW_EXTENSIONS` (7 entries)
means "**we have a decoder**", `UNSUPPORTED_EXTENSIONS` (19 entries) means "**we recognize it
and explicitly do not do it**". The moment they merge, "not supported yet" degrades into
"could not determine the format".

### 3.4 Sample matrix: a new sample is a new assertion

`samples/cameras/` (files not committed; inventory and SHA256 in that directory's README) plus
`crates/aevum-core/tests/suite/camera_samples.rs`:

- `every_camera_sample_ends_in_a_named_outcome`: every file must land on one of `Ok`
  (non-zero size) / `UnsupportedFormat` / `DecodeFailed` / `DecoderPanic`, and **the whole call
  is wrapped in `catch_unwind`** -- a catch fails the test and prints the file name. A missing
  directory **skips** (samples are not committed, that is the normal state), but **a file that
  is present and does not pass is always red**.
- `heic_is_rejected_by_name_not_guessed`: HEIC must be rejected by suffix with a name.

The shape of the assertion is deliberate: **it does not lock the error string, it locks which
class the outcome belongs to**. Locking strings welds the image crate's wording into the
assertion, so one upstream rewording forces a test change; locking the class only requires
"this thing has a name", while upstream wording changes are the business of the unit assertions
inside the decoder layer.

## 4. Why it will not happen again

Four items, ordered by "what happens if the defence breaks":

1. **A new decoder must go through the guard.** The module header of `raw/guard.rs` says why
   this boundary exists, so any newly added third-party decode call will be asked "where is the
   guard". This does not rely on discipline; it relies on reviewability -- all five call sites
   carry one line saying where their bytes come from.
2. **A new sample is a new assertion.** Drop a file into `samples/cameras/`, run the assertions
   once, and there is one more "this body / this compression must land on a named outcome".
   **Let the assertion go red first, then change code** -- doing it the other way round (change
   code, then look for a sample) is exactly how this incident happened.
3. **A missing sample skips rather than fails.** Because samples are not committed, "no
   samples" is normal and must not turn the gate red on another machine (that trains everyone
   to ignore red). But "there is a sample and it does not pass" is always red.
4. **The consequence of a panic is defined.** A worker-thread panic no longer equals "one
   silently dirty slot": either the guard turns it into an error, or the outermost layer lets
   that frame be skipped. **There is no third possibility.**

## 5. Known unsupported list (as of this round)

| Item | State | Evidence |
|---|---|---|
| HEIC / HEIF | rejected by name (`UnsupportedFormat`) | assertion `heic_is_rejected_by_name_not_guessed` |
| Fujifilm RAF, Pentax PEF, Samsung SRW, Hasselblad 3FR/FFF, Phase One IIQ, Minolta MRW, Kodak KDC/DCR/DCS/DC2, Sigma X3F, Canon CRW, Mamiya MEF, Leaf MOS, GoPro GPR | rejected by name (same table, 19 suffixes) | `raw::UNSUPPORTED_EXTENSIONS` |
| Sony `arw6` (e.g. A7V) model profile | unknown to rawler 0.8 → falls back to the embedded preview | measured in `samples/cameras` |
| Nikon HighEfficiencyStar compression | unsupported by rawler 0.8 → falls back to the embedded preview | same |
| iPhone LinearRaw DNG | rawler panics internally (caught by itself) → falls back to the embedded preview | same; this incident's crime scene |
| Sony small variant in a non-TIFF container (`IMG_9590.ARW`) | `NotTiff`; the thumbnail still comes from the complete JPEG inside the container | same |
| Pyramid method (multi-scale refinement) | not implemented | ledger section A |

## 6. What Photoshop / Lightroom show us (the part with evidence)

**Where the evidence comes from, stated up front**: the structural observations below come from reverse-engineering notes on ACR / its `cr_*.cpp` module split, **not from anything this repository re-verified**; the one claim we can prove ourselves is the last bullet -- "the container layer is per-vendor" holds on our own data too (rawler does not recognize Nikon's HighEfficiencyStar or Sony's `arw6`, which is exactly what per-model configuration looks like in practice).

ACR can "open anything" not by writing every container, but by separating two layers:

1. **Pixel semantics are unified in the container**: `DNG` normalises every vendor's sensor
   semantics through one tag set, and then **one `.dcp` per camera model** sits on top. Most of
   ACR's generality comes from here -- body differences live in data (`.dcp`), not in code
   branches.
2. **The container layer is still per-vendor**: modules such as `cr_canon_cr3.cpp` and
   `cr_read_nikon.cpp` are all still there, because CR3's ISO-BMFF structure and Nikon's
   compression each need their own parser.

Conclusion (and the basis for this round's trade-offs):

- **The container layer cannot avoid being per-vendor.** CR3 is BMFF, CRW is CIFF, X3F is a
  three-layer Foveon model; imagining one piece of code decoding all of them is a fantasy.
- What *can* be avoided is "**an unknown format cannot be opened**". The right move is
  "graceful degradation for unknown formats + sample-driven adaptation", not chasing full
  vendor support.
- So this round's action was: **get the shape of failure right** (named errors / fall back to
  the preview with an honest label / never crash) and **make "add one format" cost one sample
  plus one assertion**, instead of grinding through model tables.

## 7. What changed this round (one paragraph)

`rawler` 0.7.2 → 0.8.0; five untrusted decode boundaries plus one outermost layer all go
through `catch_unwind`; errors split into three classes (unsupported yet / decoder internal
error / specific structural problem); ten real samples plus two matrix assertions; input
hardening (1 MB plugin-manifest cap, 65535 per-side cap for plain images, TOML and serde_json
recursion limits confirmed in force).
