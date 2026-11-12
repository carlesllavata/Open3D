// SPDX-License-Identifier: GPL-3.0-or-later
//! Which package an install would use: the app's own kind, a 3ds Max bundle or a standalone app's
//! zip (`native::install::choose_kinds`, then `pick_asset`), so the UI can tell whether a release
//! has something to install.

use open3d_launcher_core::platform::pick_asset;
use open3d_launcher_core::{Asset, InstallSource, Installed, Os, PackageKind, Release, SuiteApp};

use crate::app::LauncherUi;
use crate::model::AppEntry;

/// The formats a fresh install of `app` can use on `os`, best first: its own kind of package.
fn platform_kinds(app: &SuiteApp, os: Os) -> &'static [PackageKind] {
    match os {
        Os::Windows => app.kinds(),
    }
}

/// The order a fresh install of `app` tries formats in: `preferred` first, then the rest.
fn fresh_kinds(app: &SuiteApp, os: Os, preferred: &[PackageKind]) -> Vec<PackageKind> {
    let available: Vec<PackageKind> = platform_kinds(app, os).to_vec();
    let mut kinds: Vec<PackageKind> = preferred.iter().copied().filter(|kind| available.contains(kind)).collect();
    for kind in available {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

/// The format the copy that's there was installed as: the app's own kind for one the launcher
/// installed, else what the copy was found as.
fn installed_kind(app: &SuiteApp, os: Os, source: InstallSource) -> Option<PackageKind> {
    match (os, source) {
        (Os::Windows, InstallSource::Launcher) => app.kinds().first().copied(),
        (Os::Windows, InstallSource::Bundle) => Some(PackageKind::MaxBundle),
        (Os::Windows, InstallSource::AppFolder) => Some(PackageKind::AppZip),
    }
}

/// The formats an install or update of `app` tries, best first, as `native::install::choose_kinds`
/// orders them: the installed copy's format alone when the backend surely installs it here, else
/// [`fresh_kinds`]. Empty where the UI can't tell (a fresh install on Linux).
fn install_kinds(app: &SuiteApp, os: Os, installed: Option<&Installed>, preferred: &[PackageKind]) -> Vec<PackageKind> {
    let kept = installed.and_then(|inst| installed_kind(app, os, inst.source)).filter(|kind| platform_kinds(app, os).contains(kind));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(source: InstallSource) -> Installed {
        Installed { version: None, source, location: String::new(), package: None, externally_managed: false, origin: None, installation: None }
    }

    #[test]
    fn a_plug_in_and_a_standalone_app_each_install_from_their_own_package() {
        let plugin = open3d_launcher_core::SUITE[0];
        let program = SuiteApp { id: "phototool", name: "PhotoTool", package: open3d_launcher_core::AppPackage::Standalone, ..plugin };
        assert_eq!(install_kinds(&plugin, Os::Windows, None, &[]), [PackageKind::MaxBundle]);
        assert_eq!(install_kinds(&program, Os::Windows, None, &[]), [PackageKind::AppZip]);
        // A preference saved for another format, or a copy found as something else, changes nothing.
        assert_eq!(install_kinds(&program, Os::Windows, None, &[PackageKind::MaxBundle]), [PackageKind::AppZip]);
        assert_eq!(install_kinds(&program, Os::Windows, Some(&installed(InstallSource::Launcher)), &[]), [PackageKind::AppZip]);
        assert_eq!(install_kinds(&program, Os::Windows, Some(&installed(InstallSource::AppFolder)), &[]), [PackageKind::AppZip]);
        assert_eq!(install_kinds(&plugin, Os::Windows, Some(&installed(InstallSource::Bundle)), &[]), [PackageKind::MaxBundle]);
        assert_eq!(install_kinds(&plugin, Os::Windows, Some(&installed(InstallSource::Launcher)), &[]), [PackageKind::MaxBundle]);
    }
}
