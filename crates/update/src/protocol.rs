// SPDX-License-Identifier: GPL-3.0-or-later
//! The vocabulary installers and release lookups report in: what a job is doing, how it ended,
//! where new installs go and what a release lookup found.

/// What a running job is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobPhase {
    Downloading {
        done: u64,
        total: u64,
    },
    /// The connection dropped; trying again shortly, from where it stopped.
    Reconnecting {
        done: u64,
        total: u64,
    },
    Verifying,
    /// The system's installer is busy with another app; this one goes next.
    Waiting,
    /// Waiting on the OS installer (MSI) or copying files.
    Installing,
    Removing,
}

/// The `JobFinished` error a paused job ends with (not a failure).
pub const PAUSED: &str = "paused";
/// The `JobFinished` error a cancelled job ends with (not a failure).
pub const CANCELLED: &str = "cancelled";
