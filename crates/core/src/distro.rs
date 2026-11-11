// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux distributions: reading `os-release` and choosing the package formats that suit them.
//!
//! The distro decides the system package format, not which tools happen to be on `PATH`: Fedora
//! can have `apt-get` and `dpkg` installed (for building Debian images), and an `.rpm` system
//! must never be handed to apt.
