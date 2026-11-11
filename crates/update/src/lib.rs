// SPDX-License-Identifier: GPL-3.0-or-later
//! Self-updating for Open3D Launcher: finding how the running copy is installed, downloading and
//! checking the new release, and the helper that swaps it in and restarts the launcher.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod app;
mod detect;
mod folders;
mod http;
mod install;
mod manifest;
mod msi_context;
mod msi_copy;
mod msi_migrate;
mod paths;
pub mod pins;
mod platform;
mod portable;
mod process;
mod protocol;
mod proxy;
mod release;
pub mod self_update;
pub mod suite;
mod version;

pub use app::App;
pub use http::{Fetched, Http, Stop};
pub use paths::Paths;
pub use platform::{Arch, Os, PackageKind, Platform};
pub use protocol::JobPhase;
pub use release::{Asset, Release};

/// Unix seconds now.
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
pub(crate) const TEST_AGENT: &str = "Open3D-Launcher-Test/0.0";
