// SPDX-License-Identifier: GPL-3.0-or-later
//! Which package a fresh install would use, so the install-folder fields show the folder that
//! install really goes into.
//!
//! It mirrors the backend's choice (`native::install::choose_kinds`, then `pick_asset`): the
//! preferred formats first, then every other format this platform installs, each tried against
//! every asset token. Choosing only the preferred format would show the portable `apps` folder
//! for a release that ships only an MSI, which the backend then installs into Program Files.
//! The UI cannot see a Linux machine's package managers, so there a fresh install answers nothing
//! and the field keeps saying "System default".

use open3d_launcher_core::platform::pick_asset;
use open3d_launcher_core::{Asset, InstallSource, Installed, Os, PackageKind, Release, SuiteApp};

use crate::app::LauncherUi;
use crate::model::AppEntry;

/// The formats a fresh install can use on `os`, best first.
fn platform_kinds(os: Os) -> &'static [PackageKind] {
    match os {
        Os::Windows => &[PackageKind::MaxBundle],
    }
}

/// The order a fresh install of `app` tries formats in: `preferred` first, then the rest.
fn fresh_kinds(_app: &SuiteApp, os: Os, preferred: &[PackageKind]) -> Vec<PackageKind> {
    let available: Vec<PackageKind> = platform_kinds(os).to_vec();
    let mut kinds: Vec<PackageKind> = preferred.iter().copied().filter(|kind| available.contains(kind)).collect();
    for kind in available {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

/// The format the copy that's there was installed as: always the 3ds Max bundle.
fn installed_kind(os: Os, source: InstallSource) -> Option<PackageKind> {
    match (os, source) {
        (Os::Windows, InstallSource::Launcher | InstallSource::Bundle) => Some(PackageKind::MaxBundle),
    }
}

/// The formats an install or update of `app` tries, best first, as `native::install::choose_kinds`
/// orders them: the installed copy's format alone when the backend surely installs it here, else
/// [`fresh_kinds`]. Empty where the UI can't tell (a fresh install on Linux).
fn install_kinds(app: &SuiteApp, os: Os, installed: Option<&Installed>, preferred: &[PackageKind]) -> Vec<PackageKind> {
    let kept = installed.and_then(|inst| installed_kind(os, inst.source)).filter(|kind| platform_kinds(os).contains(kind));
    match kept {
        Some(kind) => vec![kind],
        None => fresh_kinds(app, os, preferred),
    }
}

impl LauncherUi {
    /// The asset of `release` that installing or updating `entry` here would download. `None`
    /// when none would be, or the UI can't tell which (see [`install_kinds`]).
    pub(crate) fn install_asset<'r>(&self, entry: &AppEntry, release: &'r Release) -> Option<&'r Asset> {
        let platform = self.platform?;
        let kinds = install_kinds(entry.app, platform.os, entry.installed.as_ref(), &self.settings.preferred_kinds);
        let stems: Vec<&str> = entry.app.stems().collect();
        pick_asset(release, &stems, platform, &kinds).map(|(asset, _)| asset)
    }
}
