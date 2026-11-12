# Open3D

Open-source tools that make 3D work faster, starting with 3ds Max.

This repository holds the **Open3D Launcher**: one place to install, update and remove the Open3D apps and
to see what's new in each version. It is a fork of [ArtCraft Launcher](https://github.com/storytold/craft-launcher)
adapted to the Open3D apps: 3ds Max plug-ins and standalone programs (see [Credits](#credits)).

## Open3D Launcher

- Lists the Open3D apps with the version you have and the latest one, with each version's release notes.
- Installs each 3ds Max app as a plug-in bundle in `%APPDATA%\Autodesk\ApplicationPlugins`: per user, no
  administrator rights, and every 3ds Max from 2025 on loads it. **Open in 3ds Max** starts the newest 3ds Max
  you have.
- Installs each standalone app (a program of its own, such as OpenView) in
  `%LOCALAPPDATA%\Programs\Open3D\<App>`, also per user and without administrator rights, with shortcuts in
  the Start menu and on the desktop. **Open** starts it.
- Reads each app's versions from that app's own GitHub releases, and checks every download against its
  published SHA-256 before touching anything; a failed or interrupted update puts the previous version back.
- Never changes a plug-in while 3ds Max is open, nor a standalone app while it is running: close it first
  and the launcher tells you so.
- Updates itself. It keeps running in the notification area when you close its window and can start with
  Windows (both in *Preferences*).
- English and Spanish. No account, no sign-in, no telemetry.

Requirements: Windows 10 or 11 (64-bit); 3ds Max 2025 or newer for the 3ds Max apps.

## Download and install

From the [latest release](https://github.com/carlesllavata/Open3D/releases/latest) download one of:

- `open3d-launcher-<version>-windows-x64.msi`: the installer. It installs for you only, without
  administrator rights, in `%LOCALAPPDATA%\Programs\Open3D\Launcher`, with a Start menu shortcut and, if you
  tick it, a desktop one. Remove it from Windows' *Settings > Apps > Installed apps*.
- `open3d-launcher-<version>-windows-x64-portable.zip`: no installation. Unzip it anywhere and run
  `open3d-launcher.exe`; its settings stay in `Open3DLauncherData` next to it (see `portable.txt`).

The launcher isn't code-signed, so the first time Windows SmartScreen may say "Windows protected your PC":
choose **More info > Run anyway**. Updates are checked against the checksums published with each release.

Removing the launcher leaves the apps you installed with it where they are: remove them from the launcher
first if you want them gone. Removing an app keeps its settings.

> Status: early (v0.3.1). What each version adds: [CHANGELOG.md](CHANGELOG.md).

### Apps

- **OpenTools**: a tool hub for 3ds Max (scene checks and repairs, asset / proxy / XRef managers, modeling,
  material and clipboard tools). [carlesllavata/OpenTools](https://github.com/carlesllavata/OpenTools)

- **OpenManager**: scene setups and batch rendering for 3ds Max (Corona and V-Ray; reads Pulze Scene Manager
  setups). [carlesllavata/OpenManager](https://github.com/carlesllavata/OpenManager)

- **OpenElements**: renders only the render elements you tick in 3ds Max; masks and geometry elements render
  in seconds (Corona; V-Ray not tested yet). [carlesllavata/OpenElements](https://github.com/carlesllavata/OpenElements)

- **OpenView**: downloads 360 panoramas and photos from map applications at full resolution, with a 360
  viewer, crops and HDRIs. A program of its own: it doesn't need 3ds Max.
  [carlesllavata/OpenView](https://github.com/carlesllavata/OpenView)

More apps are added to the catalog as they are published.

## How it works

- **Catalog.** The list of apps is `crates/core/catalog.toml`, built into the launcher. The launcher also
  reads `launcher-catalog.toml` from this repository's `main` branch, but only when its Ed25519 signature
  (`launcher-catalog.toml.sig`) matches the public key built into the launcher
  (`crates/core/assets/launcher-catalog-public-key.pem`); otherwise it keeps the built-in list.
- **Versions.** Each app lives in its own public repository (`carlesllavata/<App>`), and each release tagged
  `vX.Y.Z` carries the app's package and `SHA256SUMS.txt`. The launcher asks GitHub for the latest release,
  at most once an hour unless you press *Check for updates*.
- **3ds Max apps.** The package is `<id>-<version>-windows-x64-3dsmax.zip`, with the `.bundle` folder at the
  zip's root; the bundle's `PackageContents.xml` must say the release's version.
- **Standalone apps.** An app the catalog marks `package = "standalone"` is a program of its own: the
  package is `<id>-<version>-windows-x64.zip` with `<App>.exe` at the zip's root, and the exe's file
  properties must say the release's version. The launcher unpacks it into
  `%LOCALAPPDATA%\Programs\Open3D\<App>`, adds Start menu and desktop shortcuts on a first install and
  removes them with the app. OpenView is the first one.
- **Launcher updates.** The launcher's own releases, in this repository, carry the MSI, the portable zip
  and `SHA256SUMS.txt`; an installed launcher downloads the same format it was installed from.

## Building from source

Needs [Rust](https://rustup.rs) (stable, MSVC toolchain) and the Visual Studio Build Tools with the C++
workload.

```
cargo build --release -p open3d-launcher     # target\release\open3d-launcher.exe
cargo test --workspace
cargo xtask ci                                # fmt --check, clippy -D warnings, tests
```

The MSI and the portable zip are made by `packaging\windows\package.ps1`, which needs
[WiX Toolset 5](https://wixtoolset.org) with its UI and Util extensions (see the script's header).

### Publishing (maintainers)

`tools\publish.py`, run with the Python that ships with 3ds Max (`python -I tools\publish.py ...`), needs
the GitHub CLI:

- `app <id>`: builds `<id>-<version>-windows-x64-3dsmax.zip` from a 3ds Max app's latest `vX.Y.Z` tag and
  attaches it, with `SHA256SUMS.txt`, to that app's GitHub release (`tools\apps.json` lists those apps). A
  standalone app builds and publishes its own zip from its repository.
- `catalog`: signs `crates\core\catalog.toml` as `launcher-catalog.toml` + `.sig` with the maintainer's
  private key (never in this repository) and publishes the app icons. Only needed when the list of apps
  changes.
- `launcher`: builds the MSI and the portable zip, tags `vX.Y.Z` and creates this repository's release
  with the version's CHANGELOG section.

Each takes `--dry-run` to build without uploading anything.

## Repository layout

| path | what it is |
|---|---|
| `apps/open3d-launcher/` | the desktop program: window, tray, single instance, logging |
| `crates/core/` | catalog, releases, versions, platforms and package formats (no I/O) |
| `crates/native/` | downloads, install / update / remove, 3ds Max bundles (`max_bundle.rs`), standalone apps (`standalone.rs`), self-update |
| `crates/ui-egui/` | the interface (egui) and its translations (`src/i18n/*.tsv`) |
| `crates/update/` | the self-update library (a modified copy of craft-update) |
| `packaging/windows/` | the MSI (WiX), the portable zip and their checks |
| `assets/` | the Open3D icon (made by `dev/make_icon.py`), the app icons and the fonts |
| `launcher-catalog.toml`, `.sig`, `launcher-app-icons/` | the signed catalog the launcher reads from `main` |
| `tools/` | publishing apps, the catalog and the launcher (maintainers) |
| `xtask/` | `cargo xtask ci`, `version` and `ico` |

## Credits

Open3D Launcher is based on [ArtCraft Launcher](https://github.com/storytold/craft-launcher) and
[craft-update](https://github.com/storytold/craft-libs) by the ArtCraft Team and their contributors,
released under the MIT License or the Apache License 2.0. Their licence texts are in [LICENSES/](LICENSES),
and [NOTICE](NOTICE) says what comes from where. ArtCraft is a trademark of the ArtCraft Team; this project
uses none of its brand assets and isn't affiliated with or endorsed by them. Fonts and other bundled
material are listed in [ATTRIBUTION.md](ATTRIBUTION.md).

## License

Copyright (C) 2026 Carles Llavata and Open3D contributors.

The Open3D Launcher is free software: you can redistribute it and/or modify it under the terms of the GNU
General Public License as published by the Free Software Foundation, either version 3 of the License, or (at
your option) any later version. It is distributed in the hope that it will be useful, but WITHOUT ANY
WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See
[LICENSE](LICENSE) for the full text of the GNU General Public License v3 (SPDX: `GPL-3.0-or-later`).
