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

The launcher is not code-signed yet, so Windows SmartScreen may warn the first time you run it.

### Apps

Apps appear here as they are published. The first ones are **OpenTools** (a tool hub for 3ds Max) and
**OpenManager** (scene setups and batch rendering).

## Building from source

Needs [Rust](https://rustup.rs) (stable, MSVC toolchain) and the Visual Studio Build Tools with the C++ workload.

```
cargo build --release
cargo test
```

Options for development:

```
open3d-launcher --catalog <path-or-url> --plugins-dir <folder>
```

`--catalog` reads another catalog (a local `catalog.json` works, with package paths relative to it) and
`--plugins-dir` installs somewhere other than the real ApplicationPlugins folder.

## Catalog format

`catalog.json` lists each app with a permanent `id`, its `bundle` folder name and its latest release:
version, date, download `url`, `sha256`, `size` and the release notes in Markdown. Packages are zips whose
root holds the bundle folder. Maintainers publish versions with `tools/publish.py`.

## License

[MIT](LICENSE)
