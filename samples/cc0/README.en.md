# Real samples (CC0)

**English** · [中文](README.md)

Synthetic fixtures can only prove "the implementation agrees with the fixture", not
"the implementation agrees with real files": both are written by the same person from
the same (possibly misremembered) reading of the spec, so a shared mistake stays green
forever. That is exactly how an off-by-one in the CFA pattern mapping survived until
2026-11-01: every conforming DNG was rejected, and only fixtures written to the same
wrong convention passed.

This directory holds files **produced by real cameras and phones**, exercised end to end.

## Licence

All files are **CC0 / public domain**. They may be redistributed with the repository and
used for any purpose with no attribution requirement. Sources and checksums are kept in
the repository purely for **traceability**: on another machine or years later, it is still
possible to confirm the exact same bytes.

## Contents

| File | Camera | Size | Bytes | SHA256 |
|---|---|---|---|---|
| `apple-iphone6s-plus/IMG_0853.DNG` | Apple iPhone 6s Plus | 4032×3024 | 10,227,467 | `845fee4f09c832af3728778737f5ae9b99124debb821b6e2066b94aed1daa91c` |

Source: <https://raw.pixls.us/data/Apple/iPhone%206s%20Plus/IMG_0853.DNG>
(the raw.pixls.us sample library, CC0 marked per file)

### Why this file is worth keeping

It trips four "only real files have this" traps, each of which made the built-in parser
fail outright before the fixes:

1. **Three-channel LinearRaw**: IFD0 is an 852×640 YCbCr preview, not sensor data;
2. **The main image lives in SubIFDs**, so reading only IFD0 yields the thumbnail;
3. **lossless JPEG with tiles** (compression=7): metadata is reportable, pixels are not --
   which is precisely the "refuse and fall back" path, so it verifies the fallback is not
   a silent swap to the preview;
4. **CFA pattern `[0,1,1,2]`**: RGGB per the spec's 0=R/1=G/2=B, so an off-by-one in the
   mapping rejects the whole file.

Assertions: `crates/aevum-core/tests/raw_real_sample.rs` (four, each mutation-tested).

## Rules for adding samples

- **CC0 or an equivalent unconditional licence**, verifiable on the file or its source
  page. No "found it online" and no vendor samples (those usually only license use with
  the vendor's own software).
- Prefer files **known to trip something**: a new camera body, a new compression, a
  non-RGGB layout. A sample that passes cleanly carries very little acceptance value.
- Keep it under 15 MB and commit the file; anything larger goes through
  `scripts/fetch-samples.sh`.
- Add a row above with the source URL and SHA256.
