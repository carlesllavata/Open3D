# Changelog

What each version of the Open3D Launcher adds, newest first.

## 0.1.0 — 11 November 2026

The first version of the launcher: one place to install, update and remove the Open3D apps for 3ds Max.

- **Install the launcher with one click.** Run the downloaded `Open3D-Launcher-0.1.0.exe` and choose
  **Install**: it goes in your user folder (no administrator rights), with a Start menu shortcut and, if you
  want, a desktop one. **Run without installing** works too.
- **Remove it like any other program**, from Windows' *Settings > Apps > Installed apps*. The apps you
  installed with it stay in 3ds Max.
- **Scripted installs:** `--install` installs the launcher without a window, and `--uninstall --quiet` removes
  it without asking.
- **The launcher updates itself.** When a newer launcher is published, a bar at the top offers to update it;
  the new version is checked before it replaces the old one, and it restarts on its own.
- **Install, update and remove apps.** The launcher lists the Open3D apps with the version you have and the
  latest one, and installs each app as a 3ds Max plug-in bundle in `ApplicationPlugins`: per user, no
  administrator rights, and every 3ds Max from 2025 on picks it up. Removing an app keeps its settings.
- **Safe updates.** Every download is checked against the catalog before anything changes; a damaged
  download is refused and the version you had stays in place. An update interrupted halfway is put right
  the next time the launcher starts.
- **3ds Max must be closed** to install, update or remove an app; the launcher says so and waits.
- **Development copies are flagged.** If a second copy of an app is in the plug-in folder (for example
  `OpenTools-dev.bundle`), the launcher warns you, because 3ds Max loads only one of them.
- **What's new in each version** is shown next to the app, taken from its changelog.
- Free software under the GNU GPL v3 or later.

The launcher isn't code-signed yet: the first time you run it, Windows SmartScreen may show "Windows
protected your PC". Choose **More info > Run anyway**.
