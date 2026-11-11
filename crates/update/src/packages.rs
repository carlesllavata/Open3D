// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading system package lists (dpkg, rpm, pacman, Flatpak) and matching them to Open3D apps.
//!
//! Pure functions over the tools' text output, so they're tested on every platform; only Linux
//! runs the tools (`detect::linux`).

use crate::{App, InstallSource, version};

/// One installed package that belongs to a Open3D app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub manager: InstallSource,
    /// The package's name (`phototool-bin`) or Flatpak app id.
    pub name: String,
    /// As the package manager reports it (`0.5.0-1`); empty when it doesn't say.
    pub version: String,
}

/// `dpkg-query -W -f '${Package}|${db:Status-Status}|${Version}\n'`: installed packages only.
pub fn parse_dpkg(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('|');
            match (f.next(), f.next(), f.next()) {
                (Some(name), Some("installed"), Some(ver)) => {
                    let name = name.split_once(':').map(|(n, _)| n).unwrap_or(name);
                    Some((name.to_string(), ver.to_string()))
                }
                _ => None,
            }
        })
        .collect()
}

/// `rpm -qa --qf '%{NAME}|%{VERSION}\n'`.
pub fn parse_rpm(text: &str) -> Vec<(String, String)> {
    text.lines().filter_map(|line| line.split_once('|').map(|(n, v)| (n.to_string(), v.to_string()))).collect()
}

/// `pacman -Q`: `name version` per line.
pub fn parse_pacman(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            Some((f.next()?.to_string(), f.next()?.to_string()))
        })
        .collect()
}

/// `flatpak list --app --columns=application,version`: the version column is often empty for
/// bundles built from a release tarball.
pub fn parse_flatpak_list(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let id = f.next()?.trim();
            (!id.is_empty()).then(|| (id.to_string(), f.next().unwrap_or("").trim().to_string()))
        })
        .collect()
}

/// The `Version:` line of `flatpak info <id>`.
pub fn parse_flatpak_info(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.trim().strip_prefix("Version:").map(|v| v.trim().to_string())).filter(|v| !v.is_empty())
}

/// The package names a Open3D app may be installed under with `manager`. On Arch the AUR carries
/// each app three ways: built from source (`phototool`), the release binary (`phototool-bin`)
/// and the latest commit (`phototool-git`). Debian and RPM packages may use either the short stem
/// (`phototool`) or the reverse-DNS bundle id (`io.github.carlesllavata.phototool`).
pub fn candidate_names(app: &App, manager: InstallSource) -> Vec<String> {
    match manager {
        InstallSource::Flatpak => vec![app.bundle_id()],
        InstallSource::Pacman => {
            app.stems().flat_map(|s| [s.to_string(), format!("{s}-bin"), format!("{s}-git")]).chain(std::iter::once(app.bundle_id())).collect()
        }
        InstallSource::Deb | InstallSource::Rpm => app.stems().map(str::to_string).chain(std::iter::once(app.bundle_id())).collect(),
        _ => app.stems().map(str::to_string).collect(),
    }
}

/// The package of `app` among `installed`, preferring the manager listed first in `order`.
pub fn find<'a>(app: &App, installed: &'a [Package], order: &[InstallSource]) -> Option<&'a Package> {
    order.iter().find_map(|manager| {
        let names = candidate_names(app, *manager);
        names.iter().find_map(|n| installed.iter().find(|p| p.manager == *manager && p.name == *n))
    })
}

/// A package's version, read the package-manager way (`0.5.0-1` is 0.5.0).
pub fn version_of(p: &Package) -> Option<version::Version> {
    version::parse_package(&p.version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_each_package_managers_listing() {
        assert_eq!(
            parse_dpkg("phototool|installed|0.5.0-1\nphototool:amd64|installed|0.6.0-1\nwordtool|config-files|0.3.0-1\n"),
            [("phototool".into(), "0.5.0-1".into()), ("phototool".into(), "0.6.0-1".into())]
        );
        assert_eq!(parse_rpm("gridtool|0.3.0\n"), [("gridtool".into(), "0.3.0".into())]);
        assert_eq!(parse_pacman("phototool-bin 0.5.0-1\nlinux 6.11.1.arch1-1\n").len(), 2);
        assert_eq!(
            parse_flatpak_list("io.github.carlesllavata.wordtool\t\nio.github.carlesllavata.cadtool\t0.3.0\n"),
            [("io.github.carlesllavata.wordtool".into(), String::new()), ("io.github.carlesllavata.cadtool".into(), "0.3.0".into())]
        );
        let info = "\nWordTool - Word processor\n\n          ID: io.github.carlesllavata.wordtool\n     Version: 0.3.0\n      Branch: master\n";
        assert_eq!(parse_flatpak_info(info).as_deref(), Some("0.3.0"));
        assert_eq!(parse_flatpak_info("Version: \n"), None);
    }
}
