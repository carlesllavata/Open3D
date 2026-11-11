// SPDX-License-Identifier: GPL-3.0-or-later
//! The Open3D suite as releases know it: each app's id, name and repository.
//!
//! An app that updates itself takes its own entry (`suite::OPENTOOLS`); the launcher takes all
//! of them. Installers use the list to tell an Open3D app's package from anyone else's.
//!
//! Based on craft-update's `suite.rs` (MIT OR Apache-2.0, see NOTICE).

use crate::app::App;

pub const OPENTOOLS: App = App::new("opentools", "OpenTools", "Tool hub for 3ds Max", "carlesllavata/OpenTools");

/// Open3D Launcher, described like an Open3D app so it updates itself with the same machinery
/// (asset names `open3d-launcher-<ver>-…`). Not part of [`SUITE`].
pub const LAUNCHER: App = App::new("open3d-launcher", "Open3D Launcher", "Installs and updates the Open3D apps for 3ds Max", "carlesllavata/Open3D");

/// The Open3D suite.
pub const SUITE: [App; 1] = [OPENTOOLS];

/// The suite's app with this id.
pub fn by_id(id: &str) -> Option<&'static App> {
    SUITE.iter().find(|a| a.id == id)
}
