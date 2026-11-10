# Changelog

What each version of the Open3D Launcher adds, newest first.

## In development

The first version of the launcher: one place to install, update and remove the Open3D apps for 3ds Max.

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
- **Free software under the GNU GPL v3 or later.** The very first public commit was under the MIT license.
