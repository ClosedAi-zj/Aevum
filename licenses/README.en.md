# Third-party licence texts

[中文](README.md) · **English**

This directory holds the verbatim licence texts of the third-party components shipped with
`aevum.exe`. **What each component is used for, its version, how it is linked, and how this
project discharges its obligations are all in `THIRD-PARTY-NOTICES.txt` at the repository
root**; this file only answers "which file belongs to which component".

The table below describes the `licenses/` directory **inside the green-package zip**. The
repository keeps only some of those files (the rest sit under `assets/` in their original
locations and are copied into `licenses/` by `scripts/package.sh` at packaging time), so it is
normal that grepping the repository does not find a given filename.

| File | Component | Licence | Distribution form |
|---|---|---|---|
| `LGPL-2.1-rawler.txt` | rawler 0.7.2 (RAW decoding backend) | LGPL-2.1 | **statically linked into `aevum.exe`** |
| `OFL-1.1-Noto-Sans-SC.txt` | Noto Sans SC | SIL OFL 1.1 | compiled into `aevum.exe` |
| `OFL-1.1-NotoEmoji.txt` | Noto Emoji | SIL OFL 1.1 | compiled into `aevum.exe` |
| `OFL-1.1-Inter.txt` | Inter | SIL OFL 1.1 | compiled into `aevum.exe` |
| `UFL-1.0-Ubuntu-Light.txt` | Ubuntu Light (one of the egui default fonts) | Ubuntu Font Licence 1.0 | compiled into `aevum.exe` |
| `Hack-Regular.txt` | Hack (one of the egui default fonts) | MIT (plus DejaVu public domain and Bitstream Vera) | compiled into `aevum.exe` |
| `egui-icons.txt` | egui-icons.ttf (a five-glyph subset of emoji-icon-font) | MIT (same as emoji-icon-font) | compiled into `aevum.exe` |
| `emoji-icon-font-MIT.txt` | emoji-icon-font | MIT | compiled into `aevum.exe` (under the `default_fonts` feature only the egui-icons subset may ship) |
| `ISC-Lucide.txt` | Lucide Icons (the SVGs under `assets/icons/`) | ISC | distributed as source with the repository, not compiled into the exe |

> Filenames are prefixed with the **licence identifier**, not the component name, so you can
> tell at a glance which obligation each file discharges: `LGPL-2.1-*`, `OFL-1.1-*`,
> `UFL-1.0-*`, `ISC-*`. The unprefixed ones (`Hack-Regular.txt`, `egui-icons.txt`,
> `emoji-icon-font-MIT.txt`) keep the names their upstream releases used — renaming them would
> make them unmatchable. The packaging script carries a path mapping
> (`assets/fonts/OFL-NotoSansSC.txt` → `licenses/OFL-1.1-Noto-Sans-SC.txt`), so renaming a file
> means changing both places.

> **The two Noto OFL texts have identical contents** — that is not a copy-paste mistake: both
> are the OFL 1.1 licence body (without the copyright header). The "copyright notice" OFL asks
> for lives in each font file's own `name` table and travels inside the font binary — this
> project modifies no font file, so those notices are preserved verbatim in the binary. Two
> filenames simply make it obvious which components a given licence covers. The Inter copy
> (`OFL-1.1-Inter.txt`) carries an extra `Copyright 2020 The Inter Project Authors` line,
> which is how its upstream release formats it.

## Where the source files live (original repository locations)

| Component | Repository location | After packaging |
|---|---|---|
| Inter font | `assets/fonts/OFL-Inter.txt` | `licenses/OFL-1.1-Inter.txt` |
| Noto Sans SC font | `assets/fonts/OFL-NotoSansSC.txt` | `licenses/OFL-1.1-Noto-Sans-SC.txt` |
| Lucide icons | `assets/icons/LICENSE-Lucide.txt` | `licenses/ISC-Lucide.txt` |
| rawler / egui default fonts, etc. | this directory | copied verbatim |

The mapping rules live in the "licence texts" section of `scripts/package.sh`; the table above
only documents them. **Changing one side means changing the other** — this table once spelled
`OFL-1.1-Noto-Sans-SC.txt` as `OFL-1.1-NotoSansSC.txt` (two hyphens short), and following it
into the package meant not finding the file.

## Why the full GPLv3 text for FFmpeg is not copied in here

This section exists because the omission is easy to misread as an oversight. The reason: what
we distribute is an **unmodified standalone program**. The full GPLv3 text (~35 KB) and the
source-availability route are already stated in section 1 of `THIRD-PARTY-NOTICES.txt` and in
the bundled `tools/README-ffmpeg.txt`. **The licence requires that licence and source
information be provided, not that the full text be copied**; FFmpeg's own distribution points
to links as well. If your organisation requires the full text in the bundle, drop the GPLv3
text into this directory — it affects no build.
