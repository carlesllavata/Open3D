# Open3D

Open-source tools that make 3D work faster, starting with 3ds Max.

This repository holds the **Open3D Launcher** and the catalog it reads: one place to install, update and
remove the Open3D apps and to see what's new in each version.

## Open3D Launcher

- Lists the Open3D apps with the version you have and the latest one.
- Installs each app as a 3ds Max plug-in bundle in `%APPDATA%\Autodesk\ApplicationPlugins`: per user, no
  administrator rights, and every 3ds Max from 2025 on picks it up.
- Updates in place and checks every download against the catalog's checksum before touching anything.
- Never changes anything while 3ds Max is open, and never removes your settings when you uninstall an app.
- No account, no sign-in.

Requirements: Windows 10 or 11 (64-bit), 3ds Max 2025 or newer.

> Status: in development. There is no downloadable release yet: build it from source (below). Once
> released, it will not be code-signed at first, so Windows SmartScreen may warn the first time you run it.
> What each version adds: [CHANGELOG.md](CHANGELOG.md).

### Apps

The catalog is empty for now: apps are added to it as they are published. The first ones will be
**OpenTools** (a tool hub for 3ds Max) and **OpenManager** (scene setups and batch rendering).

## Building from source

Needs [Rust](https://rustup.rs) (stable, MSVC toolchain) and the Visual Studio Build Tools with the C++ workload.

```
cargo build --release      # target\release\open3d-launcher.exe
cargo test
```

Options for development (`--help` lists them):

```
open3d-launcher --catalog <path-or-url> --plugins-dir <folder>
```

`--catalog` reads another catalog instead of the public one (a local `catalog.json` works, with package
paths relative to it) and `--plugins-dir` installs somewhere other than the real ApplicationPlugins folder.

## Repository layout

| path | what it is |
|---|---|
| `src/` | the launcher (Rust + egui): catalog, downloads, install / update / remove, window, release notes |
| `catalog.json` | the public catalog the launcher reads from this repo's `main` branch |
| `tools/publish.py`, `tools/apps.json` | publishing an app version (maintainers) |
| `dev/make_test_catalog.ps1` | a local test catalog built from the app checkouts (maintainers) |
| `CHANGELOG.md` | what each version of the launcher adds |

## Catalog format

`catalog.json` lists each app with a permanent `id`, its `bundle` folder name and its latest release:
version, date, download `url`, `sha256`, `size` and the release notes in Markdown. Packages are zips whose
root holds the bundle folder, attached to releases of this repository tagged `<id>-vX.Y.Z`.

Maintainers publish a version with `tools/publish.py <id>` (run with the Python that ships with 3ds Max;
needs the GitHub CLI). It builds the package from the app's release tag, creates the release and updates
`catalog.json`; `--dry-run` builds the package without uploading anything.

## License

Copyright (C) 2026 Carles Llavata and Open3D contributors.

The Open3D Launcher is free software: you can redistribute it and/or modify it under the terms of the GNU General
Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option)
any later version. It is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even
the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See [LICENSE](LICENSE) for the full
text of the GNU General Public License v3 (SPDX: `GPL-3.0-or-later`).
