// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading system package lists (dpkg, rpm, pacman, Flatpak) and matching them to Open3D apps.
//!
//! Pure functions over the tools' text output, so they're tested on every platform; only Linux
//! runs the tools (`detect::linux`).

use open3d_launcher_core::{InstallSource, SuiteApp, version};

/// One installed package that belongs to a Open3D app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub manager: InstallSource,
    /// The package's name (`phototool-bin`) or Flatpak app id.
    pub name: String,
    /// As the package manager reports it (`0.5.0-1`); empty when it doesn't say.
    pub version: String,
    /// A per-user Flatpak install (`flatpak --user`), which the launcher installs and updates.
    pub user: bool,
    /// The origin remote (`flatpark`, `<app>-origin`, `flathub`), when known.
    pub origin: Option<String>,
    /// The installation scope or name (`user`, `system`, or a named installation), for Flatpak installs.
    pub installation: Option<String>,
}

/// The `flatpak` argument that selects an installation (`user`, `system`, or a named one).
pub fn flatpak_scope(installation: Option<&str>) -> String {
    match installation {
        Some("user") => "--user".into(),
        Some("system") | None => "--system".into(),
        Some(name) => format!("--installation={name}"),
    }
}

/// The package names a Open3D app may be installed under with `manager`. On Arch the AUR carries
/// each app three ways: built from source (`phototool`), the release binary (`phototool-bin`)
/// and the latest commit (`phototool-git`). Debian and RPM packages may use either the short stem
/// (`phototool`) or the reverse-DNS bundle id (`io.github.carlesllavata.phototool`).
pub fn candidate_names(app: &SuiteApp, manager: InstallSource) -> Vec<String> {
    match manager {
        InstallSource::Flatpak => app.bundle_ids().collect(),
        InstallSource::Pacman => {
            app.stems().flat_map(|s| [s.to_string(), format!("{s}-bin"), format!("{s}-git")]).chain(std::iter::once(app.bundle_id())).collect()
        }
        InstallSource::Deb | InstallSource::Rpm => app.stems().map(str::to_string).chain(std::iter::once(app.bundle_id())).collect(),
        _ => app.stems().map(str::to_string).collect(),
    }
}

/// The package of `app` among `installed`, preferring the manager listed first in `order`. A user
/// install beats a system one under any of the app's ids (#881); within a scope the current id wins.
pub fn find<'a>(app: &SuiteApp, installed: &'a [Package], order: &[InstallSource]) -> Option<&'a Package> {
    order.iter().find_map(|manager| {
        let names = candidate_names(app, *manager);
        [true, false]
            .into_iter()
            .find_map(|user| names.iter().find_map(|n| installed.iter().find(|p| p.manager == *manager && p.user == user && p.name == *n)))
    })
}

/// A package's version, read the package-manager way (`0.5.0-1` is 0.5.0).
pub fn version_of(p: &Package) -> Option<version::Version> {
    version::parse_package(&p.version)
}

#[cfg(test)]
mod remote_url_tests {}
