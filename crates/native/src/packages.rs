// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading system package lists (dpkg, rpm, pacman, Flatpak) and matching them to Open3D apps.
//!
//! Pure functions over the tools' text output, so they're tested on every platform; only Linux
//! runs the tools (`detect::linux`).
