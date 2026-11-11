// SPDX-License-Identifier: GPL-3.0-or-later
//! Open3D Launcher's desktop backend.
//!
//! [`NativeBackend`] runs every slow thing (HTTP, scanning disks, installers) on worker threads
//! and reports back through [`Event`](open3d_launcher_core::protocol::Event)s, so the UI never
//! waits on it. Release information comes from a [`source::ReleaseSource`]: GitHub today, the
//! Open3D catalog API later.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod backend;
mod detect;
mod feed_throttle;
mod hosted_catalog;
mod http;
pub mod install;
pub mod login;
mod manifest;
mod max_bundle;
#[cfg(any(windows, test))]
mod msi_context;
pub mod paths;
mod pins;
#[cfg(windows)]
mod portable;
pub mod process;
pub mod proxy;
mod screenshots;
pub mod self_update;
pub mod single_instance;
mod source;

pub use backend::NativeBackend;

/// Unix seconds now.
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
