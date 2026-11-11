// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading a program's version from its VERSIONINFO, as the launcher does for a portable app it
//! has just unpacked: here, from the launcher's own exe, which `build.rs` gives a VERSIONINFO.

#![cfg(windows)]
