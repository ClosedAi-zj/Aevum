# Camera samples (not committed)

[中文](README.md) · **English**

The samples in `samples/cc0/` are CC0 and can be distributed with the repository; **this
directory is different**. These files are real camera output provided by the user, **partly
of unknown provenance and large**, so they are **not distributed with the repository**
(`.gitignore` ignores the whole directory and lets through only these two READMEs). To run
the matrix assertions on another machine you must copy the files in by hand.

Why this layer has to exist: the iPhone DNG in `samples/cc0/` is **one** model with **one**
way of writing files. The first run of the 1.5 candidate against ten real samples caught
`rawler 0.7.2` reading out of bounds on an iPhone LinearRaw DNG -- and the panic happened
inside rawler's own rayon worker, was swallowed, and the main flow returned `Ok` with
partially un-linearised garbage. **One sample cannot carry the word "compatibility".**
Root cause and defences: `docs/RAW兼容性-根因与防线.en.md`.

## Inventory and checksums

SHA256 values were computed with `sha256sum` (after moving machines, verify them first to
confirm you have the same byte sequences). Every "measured result" below was **actually run
this round**, not copied from a plan.

| File | Camera | Size | Bytes | SHA256 | Measured result |
|---|---|---|---|---|---|
| `Canon - EOS R6 Mark III - RAW (3_2).CR3` | Canon EOS R6 Mark III | 6982×4654 | 34,608,340 | `e491e4bb960961b5fa299361bf698310a80cbe7b15d30d8dad3bb21bc5457dab` | **OK**, rawler sensor decode (`RawPath::Sensor`) |
| `IMG_1159.HEIC` | iPhone (HEIC container) | — | 2,214,202 | `b7ad723562bd3d8b16bde1c62ce866bc71d1c3004843bff075fc3e7b962e3eba` | **UnsupportedFormat** (rejected by name, as intended) |
| `IMG_1381.DNG` | iPhone (LinearRaw) | 4032×3024 | 12,776,150 | `dda4e93555cc3ce8f6978a9a3283a6df4ca8a33231847c30136fc25059a6bf6a` | **OK, but via the embedded preview**: rawler 0.8.0 catches its own panic and reports "possibly corrupt image", routing falls back to the built-in path, which yields the camera JPEG preview (`RawPath::Preview`). No crash, no dirty data |
| `IMG_9590.ARW` | Sony (small 1.7 MB variant) | — | 1,700,212 | `b5730ac0499335501eb46ccfa951d6906e522fb6a951b4487dbfda764899b3a4` | **DecodeFailed**: not a TIFF-family container (`NotTiff`) and rawler does not recognize the model. The thumbnail still works -- the container carries one complete JPEG, found by the preview scan |
| `Nikon - Z6_3 - 8bit compressed (3_2).NEF` | Nikon Z6III | 3984×2656 | 9,169,408 | `1c7be7b9081fcbd8bb75b28a8432531fae4dc56acda6b7f41a32625fb2555600` | **OK, via the embedded preview**: rawler reports `NEF compression HighEfficencyStar is not supported`, so the built-in path takes over |
| `PANA0001.RW2` | Panasonic | 6000×4000 | 29,946,368 | `8f89fbd6b52254052b36ea074262c16ad50e38ece42c52806e55938414a5c061` | **OK**, rawler sensor decode |
| `PANA0198.JPG` | Panasonic | 6000×4000 | 12,360,704 | `31940d4244df4fbb322da39d4e486b244583396bdbb768772cd7bdb30952511f` | **OK**, plain image path (`image` crate) |
| `PANA0450.RW2` | Panasonic | 6000×4000 | 27,850,752 | `1a635cd69b945e729006b5c5e084ddb924e154ca95c5287cda045126df46c140` | **OK**, rawler sensor decode |
| `PANA1631.RW2` | Panasonic (portrait) | 6000×4000 | 29,060,096 | `061f9c513a307aa387a3a4aee66c33ee9061c0355d7a3bff30a7d63ec23f3c0e` | **OK**, rawler sensor decode (orientation applied from EXIF) |
| `Sony - ILCE-7M5 - 14bit (3_2).ARW` | Sony A7V | 4608×3072 | 12,103,680 | `8315af2a9889709f4c836dbaadc23d226847e5a0630fda212a9f896d5fe0569f` | **OK, via the embedded preview**: rawler reports `Unknown camera, model 'ILCE-7M5', mode: 'arw6'` (no profile for this body yet), so the built-in path takes over |

To re-check: `cd samples/cameras && sha256sum *` and compare row by row.

## What each sample uniquely tests

| Sample | The one thing only it can test |
|---|---|
| CR3 (Canon, 34 MB) | **ISO-BMFF container + CRX compression.** A non-TIFF container whose orientation tag lives only in the container, where `exif::read` cannot see it |
| NEF (Nikon, 8-bit compressed) | **A new compression scheme** (HighEfficiencyStar): when it cannot be decoded the code must fall back to the preview *and say so*, never pretend success |
| ARW ×2 (two Sony generations) | **Two eras from one vendor**: the older one decodes, the A7V's `arw6` is not known yet -- exactly the real-world boundary of a model-table-driven decoder |
| RW2 ×3 (Panasonic) | **14-bit vendor packing**, plus portrait orientation (PANA1631 is 3:4 upright) |
| DNG (iPhone LinearRaw) | **Three-channel LinearRaw plus a file that is known to panic rawler out of bounds**: the crime scene of this round, locking in "a panic must not escape" |
| HEIC | **An explicit negative**: it must be reported as a named "not supported yet" and must not fall into "could not determine the format" |
| JPG (PANA0198) | **The plain image path is not hijacked by the RAW dispatch** (6000×4000 decoded straight by the `image` crate) |

## Assertions

`crates/aevum-core/tests/suite/camera_samples.rs`:

- `every_camera_sample_ends_in_a_named_outcome`: walks this directory (skipping the
  READMEs) and runs `metrics::decode` on each file. It **never panics** (the whole call is
  wrapped in `catch_unwind`; a catch fails the test and prints the file name), and the result
  must be either `Ok` (with a non-zero size) or one of three named errors:
  `UnsupportedFormat` / `DecodeFailed` (container or compression not supported) /
  `DecoderPanic`. When the directory is missing or empty the test prints a note and
  **skips** -- samples are not committed, so that is a normal state, not a failure.
- `heic_is_rejected_by_name_not_guessed`: HEIC must be `UnsupportedFormat` with `heic` in the
  message.

## Committing rules (the opposite of `samples/cc0/`)

- **Files are never committed**, only this README and its `.md` twin.
- After adding a sample: update the table above (size/bytes/SHA256/result, all measured on
  the spot) and run the assertions.
- To turn "not supported" into "supported", put the file here first and let the assertion go
  red once, *then* change the code. Doing it the other way round (change the code, then look
  for a sample) is exactly how this round's incident happened.
