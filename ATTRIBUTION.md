# Attribution

Every non-code asset in this repository, with its author, source and license. The launcher's code is
GPL-3.0-or-later ([`LICENSE`](LICENSE)); the parts that come from ArtCraft Launcher and craft-update keep
their MIT OR Apache-2.0 notices ([`NOTICE`](NOTICE), [`LICENSES/`](LICENSES)). When you add an asset, add a
row here in the same change; third-party assets must be under a GPL-compatible license and keep their
license file next to them.

## Bundled in the app

| Path | Title | Author | Source | License |
|---|---|---|---|---|
| `assets/open3d/` (all files) | Open3D cube: the launcher's window, tray, exe and installer icon | Carles Llavata | Drawn by [`dev/make_icon.py`](dev/make_icon.py); `icon.svg` drawn by hand with the same geometry | GPL-3.0-or-later |
| `assets/app-icons/opentools.png` | OpenTools icon in the launcher (for now the Open3D cube) | Carles Llavata | Copy of `assets/open3d/icon-256.png` | GPL-3.0-or-later |
| `assets/fonts/Inter-Variable.ttf` | Inter 4.001, variable (body text) | The Inter Project Authors (Rasmus Andersson) | Google Fonts, <https://github.com/google/fonts/tree/main/ofl/inter> (`Inter[opsz,wght].ttf`, renamed) | SIL OFL 1.1, [`assets/fonts/OFL-Inter.txt`](assets/fonts/OFL-Inter.txt) |
| `assets/fonts/Archivo-Variable.ttf` | Archivo 2.001, variable (headings and app names) | The Archivo Project Authors (Omnibus-Type) | Google Fonts, <https://github.com/google/fonts/tree/main/ofl/archivo> (`Archivo[wdth,wght].ttf`, renamed) | SIL OFL 1.1, [`assets/fonts/OFL-Archivo.txt`](assets/fonts/OFL-Archivo.txt) |
| `assets/fonts/GeistMono-Variable.ttf` | Geist Mono 1.701, variable (labels and buttons) | The Geist Project Authors (Vercel) | Google Fonts, <https://github.com/google/fonts/tree/main/ofl/geistmono> (`GeistMono[wght].ttf`, renamed) | SIL OFL 1.1, [`assets/fonts/OFL-GeistMono.txt`](assets/fonts/OFL-GeistMono.txt) |
| (dependency) `epaint_default_fonts/fonts/NotoEmoji-Regular.ttf` | Noto Emoji Regular, monochrome emoji fallback for arbitrary release-note, feed and alert text | The Noto Project Authors (Google) | Bundled transitively by `egui 0.37`'s `monochrome_emoji_fonts` feature from `epaint_default_fonts`; <https://github.com/storytold/egui/tree/6d518fc799681571963842f95b55a6b68431b15a/crates/epaint_default_fonts/fonts> | SIL OFL 1.1, `epaint_default_fonts/fonts/OFL.txt`; see also the dependency `README` and egui `README.md` |
| (dependency) `epaint_default_fonts/fonts/emoji-icon-font.ttf` | emoji-icon-font, monochrome symbol/emoji fallback | John Slegers | Bundled transitively by `egui 0.37`'s `monochrome_emoji_fonts` feature from `epaint_default_fonts`; <https://github.com/jslegers/emoji-icon-font> | MIT, Copyright (c) 2014 John Slegers, `epaint_default_fonts/fonts/emoji-icon-font-mit-license.txt` |
| (dependency) `epaint_default_fonts/fonts/egui-icons.ttf` | egui-icons, five-glyph subset of emoji-icon-font | John Slegers | [egui 6d518fc7](https://github.com/storytold/egui/tree/6d518fc799681571963842f95b55a6b68431b15a/crates/epaint_default_fonts/fonts); included by default_fonts independently of monochrome_emoji_fonts | MIT, Copyright (c) 2014 John Slegers; full upstream text in `NOTICE` |
| (dependency) `epaint_default_fonts/fonts/Ubuntu-Light.ttf` | Ubuntu Light, default font fallback | Canonical Ltd. | [Pinned egui source](https://github.com/storytold/egui/tree/6d518fc799681571963842f95b55a6b68431b15a/crates/epaint_default_fonts/fonts) | Ubuntu Font Licence 1.0; full upstream text and copyright in `NOTICE` |
| (dependency) `epaint_default_fonts/fonts/Hack-Regular.ttf` | Hack Regular, default monospace fallback | Source Foundry Authors; Bitstream Inc.; DejaVu contributors | [Pinned egui source](https://github.com/storytold/egui/tree/6d518fc799681571963842f95b55a6b68431b15a/crates/epaint_default_fonts/fonts) | MIT and Bitstream Vera License; DejaVu work public domain; full upstream notices in `NOTICE` |

## Hosted catalog verification

| Path | Title | Author | Source | License |
|---|---|---|---|---|
| `crates/core/assets/launcher-catalog-public-key.pem` | Launcher catalog Ed25519 verification key | Carles Llavata | Public half of the maintainer's catalog signing key (the private half is never in this repository) | GPL-3.0-or-later |
| `crates/core/catalog.toml`, `launcher-catalog.toml` | Bundled and published launcher catalog | Carles Llavata | Written for Open3D; `launcher-catalog.toml` is the signed copy published by `tools/publish.py catalog` | GPL-3.0-or-later |
| `launcher-catalog.toml.sig`, `crates/core/tests/fixtures/hosted-catalog.toml.sig` | Detached catalog signatures | Carles Llavata | Made with the maintainer's catalog signing key | GPL-3.0-or-later |