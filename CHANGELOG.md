# Changelog

What each version of the Open3D Launcher adds, newest first.

## In development

- **OpenElements is no longer in the launcher: it is now the Render Elements tool of OpenTools.**
  Install or update OpenTools to get it. If you installed OpenElements, uninstall it from the launcher
  (or delete `OpenElements.bundle` from ApplicationPlugins with 3ds Max closed).

## 0.3.1 — 12 November 2026

- **OpenElements is in the launcher.** Install and update OpenElements (render only the render elements you
  choose) from the launcher, like the other 3ds Max apps.
- **Only one launcher at a time, again.** Another program on your computer could use the same local port as the
  launcher, and then a second launcher could open next to the first. The launcher now has a port of its own, and
  asks a launcher up to 0.3.0 that is still running to make way.
- **Release notes read better.** Bold text shows in bold instead of between asterisks, and a change that runs
  over several lines stays one item of the list.

## 0.3.0 — 12 November 2026

- **OpenView is in the launcher.** Install, update and open OpenView (360 panoramas and photos from map
  applications at full resolution, with a viewer, crops and HDRIs) from the launcher. It is the first app
  that is a program of its own: it doesn't need 3ds Max.
- **Standalone apps.** The launcher can now install, update, open and remove an app that is a program of
  its own rather than a 3ds Max plug-in. It goes in `Programs\Open3D\<App>` in your user folder, without
  administrator rights, with shortcuts in the Start menu and on the desktop, and **Open** starts it.
  Downloads are checked and updates put in place as for plug-ins: the version you have stays until the new
  one is ready, and comes back if the update fails. Close the app before updating or removing it; the
  launcher tells you when it is still open. Removing it keeps its settings.

## 0.2.1 — 11 November 2026

- **OpenManager is in the launcher.** Install and update OpenManager (scene setups and batch rendering)
  from the launcher, like OpenTools.
- **A warning for duplicate copies of an app.** When 3ds Max's plug-in folder holds another copy of an
  app, such as a development bundle (`OpenTools-dev.bundle` next to `OpenTools.bundle`), the app's card
  names it and reminds you that 3ds Max loads only one of them.
- **Going back to an older version works.** Choosing an older version of an app was always refused; the
  plug-in you have now stays in place until the older one is installed, so nothing is lost if it fails.
- **The launcher's own versions show in Preferences.** The list of launcher versions was empty on
  Windows.
- **A smaller, simpler launcher.** Code it inherited for other systems (macOS, Linux) and for
  install formats Open3D doesn't use is gone, along with the install-format and install-folder
  settings, which never applied to 3ds Max plug-ins.

## 0.2.0 — 11 November 2026

A new launcher, rebuilt on the open-source code of [ArtCraft Launcher](https://github.com/storytold/craft-launcher)
and adapted to 3ds Max. It doesn't update from 0.1.x on its own: remove the old launcher from Windows'
*Settings > Apps > Installed apps* and install this one. The apps you installed stay where they are and the
new launcher finds them.

- **Installer or portable, your choice.** `open3d-launcher-0.2.0-windows-x64.msi` installs the launcher for
  you only (no administrator rights) with a setup wizard, a Start menu shortcut and an optional desktop one.
  The portable zip runs from any folder and keeps its settings next to it.
- **Each app's versions come from its own GitHub repository**, OpenTools from
  [carlesllavata/OpenTools](https://github.com/carlesllavata/OpenTools). Every download is checked against the
  SHA-256 published with the release before anything is installed.
- **Install any published version** of an app, not just the latest, and **update all** your apps at once.
- **Open in 3ds Max** starts the newest 3ds Max on your computer.
- **3ds Max must be closed** to install, update or remove an app; the launcher says so instead of failing
  halfway. An interrupted update puts the previous version back.
- **A signed catalog.** The list of apps is built into the launcher and can be extended from this
  repository only with a matching signature.
- **Grid or table view, light or dark theme**, and release notes for every version.
- **Notification area and start with Windows:** closing the window keeps the launcher running in the
  notification area, where it can tell you about updates; both are optional in *Preferences*.
- **English and Spanish.** The launcher follows Windows' language; change it in *Preferences*.
- **The launcher updates itself** from the same kind of package you installed (MSI or portable). It isn't
  code-signed yet, so each update is checked against the release's published checksum.
- Free software under the GNU GPL v3 or later. ArtCraft Launcher's MIT / Apache-2.0 notices are kept in
  `NOTICE` and `LICENSES/`.

## 0.1.2 — 11 November 2026

- **New app versions now really show up within a minute.** The launcher reads the catalog through GitHub's API,
  whose cache lasts at most a minute; if that fails, it reads it the old way. The fix in 0.1.1 didn't work:
  GitHub's 5-minute cache for the catalog file ignores the trick 0.1.1 used.

## 0.1.1 — 11 November 2026

- **New app versions show up right away.** The launcher now always reads the current catalog: before, a
  version published a moment ago could take up to five minutes to appear, because GitHub keeps the catalog
  in a short-lived cache.

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
