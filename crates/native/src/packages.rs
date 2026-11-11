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

/// `dpkg-query -S <path>`: the package that owns `path`, from the line that names `path` itself.
/// An owning line starts with the package (a multiarch arch qualifier like `art-tool:amd64`
/// included, stripped here), while `diversion by … from:`/`to:` lines name the path without owning
/// it — and a package name never contains a space or a colon beyond that qualifier.
pub fn parse_dpkg_search(text: &str, path: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (package, file) = line.split_once(": ")?;
        let package = package.split(':').next().filter(|p| !p.is_empty() && !p.contains(char::is_whitespace))?;
        (file == path).then(|| package.to_string())
    })
}

/// `rpm -qf <path> --qf '%{NAME}|%{VERSION}\n'`: the package that owns the path, with its
/// version. A path no package owns makes `rpm` exit with an error, so no line ever arrives.
pub fn parse_rpm_owner(text: &str) -> Option<(String, String)> {
    let (name, version) = text.lines().next()?.split_once('|')?;
    (!name.is_empty() && !version.is_empty()).then(|| (name.to_string(), version.to_string()))
}

/// `pacman -Qo <path>`: `<path> is owned by <package> <version>`. A path no package owns makes
/// `pacman` exit with an error, so no line ever arrives.
pub fn parse_pacman_owner(text: &str) -> Option<(String, String)> {
    let (name, version) = text.lines().next()?.split_once(" is owned by ")?.1.split_once(' ')?;
    (!name.is_empty() && !version.is_empty()).then(|| (name.to_string(), version.to_string()))
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

/// `flatpak list --app --columns=application,version,origin,installation`: parses id, version, origin, and installation.
pub fn parse_flatpak_list_with_origin(text: &str) -> Vec<(String, String, Option<String>, Option<String>)> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let id = f.next()?.trim();
            if id.is_empty() {
                return None;
            }
            let version = f.next().unwrap_or("").trim().to_string();
            let origin = f.next().map(str::trim).filter(|o| !o.is_empty()).map(ToString::to_string);
            // A named installation lists as `system (<name>)`, but `--installation=` takes the bare name.
            let installation = f
                .next()
                .map(str::trim)
                .filter(|i| !i.is_empty())
                .map(|i| i.strip_prefix("system (").and_then(|n| n.strip_suffix(')')).unwrap_or(i).to_string());
            Some((id.to_string(), version, origin, installation))
        })
        .collect()
}

/// `flatpak list --app --columns=application,version`: the version column is often empty for
/// bundles built from a release tarball.
pub fn parse_flatpak_list(text: &str) -> Vec<(String, String)> {
    parse_flatpak_list_with_origin(text).into_iter().map(|(id, v, _, _)| (id, v)).collect()
}

/// Whether an origin string indicates an install from FlatPark.
pub fn is_flatpark_origin(origin: &str) -> bool {
    origin.trim().eq_ignore_ascii_case("flatpark")
}

/// The `flatpak` argument that selects an installation (`user`, `system`, or a named one).
pub fn flatpak_scope(installation: Option<&str>) -> String {
    match installation {
        Some("user") => "--user".into(),
        Some("system") | None => "--system".into(),
        Some(name) => format!("--installation={name}"),
    }
}

/// The `Version:` line of `flatpak info <id>`.
pub fn parse_flatpak_info(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.trim().strip_prefix("Version:").map(|v| v.trim().to_string())).filter(|v| !v.is_empty())
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

    #[test]
    fn an_ownership_search_names_only_a_line_that_owns_the_path() {
        assert_eq!(parse_dpkg_search("art-tool: /usr/bin/arttool\n", "/usr/bin/arttool").as_deref(), Some("art-tool"));
        assert_eq!(parse_dpkg_search("art-tool:amd64: /usr/bin/arttool\n", "/usr/bin/arttool").as_deref(), Some("art-tool"));
        // A diversion names the path without owning it — a package diversion says who, a local
        // one doesn't (dpkg 1.22.6 wording, as observed on Ubuntu 24.04).
        assert_eq!(parse_dpkg_search("diversion by art-tool from: /usr/bin/arttool\n", "/usr/bin/arttool"), None);
        assert_eq!(
            parse_dpkg_search("local diversion from: /usr/bin/arttool\nlocal diversion to: /usr/bin/arttool.real\n", "/usr/bin/arttool.real"),
            None
        );
        assert_eq!(parse_dpkg_search("other: /usr/bin/other\n", "/usr/bin/arttool"), None);
        assert_eq!(parse_dpkg_search("", "/usr/bin/arttool"), None);
        assert_eq!(parse_rpm_owner("art-tool|0.41.1-1.fc44\n"), Some(("art-tool".into(), "0.41.1-1.fc44".into())));
        assert_eq!(parse_rpm_owner("art-tool|\n"), None);
        assert_eq!(parse_rpm_owner(""), None);
        assert_eq!(parse_pacman_owner("/usr/bin/arttool is owned by art-tool 0.41.1-1\n"), Some(("art-tool".into(), "0.41.1-1".into())));
        assert_eq!(parse_pacman_owner("error: No package owns /usr/bin/arttool\n"), None);
    }

    #[test]
    fn parses_flatpak_list_with_origin_and_spots_flatpark() {
        let text = "io.github.carlesllavata.phototool\t0.19.0\tflatpark\tuser\nio.github.carlesllavata.cadtool\t0.16.0\tcadtool-origin\tsystem\nio.github.carlesllavata.wordtool\t0.14.0\t\t\nio.github.carlesllavata.filmtool\t0.3.0\tflatpark\tsystem (extra)\n";
        let list = parse_flatpak_list_with_origin(text);
        assert_eq!(
            list,
            [
                ("io.github.carlesllavata.phototool".into(), "0.19.0".into(), Some("flatpark".into()), Some("user".into())),
                ("io.github.carlesllavata.cadtool".into(), "0.16.0".into(), Some("cadtool-origin".into()), Some("system".into())),
                ("io.github.carlesllavata.wordtool".into(), "0.14.0".into(), None, None),
                ("io.github.carlesllavata.filmtool".into(), "0.3.0".into(), Some("flatpark".into()), Some("extra".into())),
            ]
        );
        assert_eq!(flatpak_scope(Some("extra")), "--installation=extra");
        assert_eq!(flatpak_scope(Some("user")), "--user");
        assert_eq!(flatpak_scope(None), "--system");
        assert!(is_flatpark_origin("flatpark"));
        assert!(is_flatpark_origin("FlatPark"));
        assert!(!is_flatpark_origin("flatpark-origin"));
        assert!(!is_flatpark_origin("cadtool-origin"));
        assert!(!is_flatpark_origin("flathub"));
        assert!(!is_flatpark_origin(""));
    }
}

/// Matches the OCI URL from https://github.com/carlesllavata/Open3D/arttool-oci.flatpakrepo,
/// independently of its local remote name (normally `arttool-oci`).
pub fn is_open3d_remote(origin: &str, remotes: &str) -> bool {
    remotes.lines().any(|line| {
        let Some((name, url)) = line.split_once('\t') else { return false };
        let url = url.trim().trim_end_matches('/');
        name.trim() == origin && url == "oci+https://github.com/carlesllavata/Open3D"
    })
}

/// Where Flatpak finds the Open3D OCI images' signatures (arttool-oci.flatpakrepo's
/// `SignatureLookaside`), redirected to each release's `.oci.sig` asset.
pub const OPEN3D_SIGNATURE_LOOKASIDE: &str = "https://github.com/carlesllavata/Open3D/signatures";

/// Did an update fail only because `origin`, an Open3D OCI remote, has no signature lookaside
/// (a signed bundle's origin, which `flatpak build-bundle` can't give one)?
pub fn needs_signature_lookaside(stderr: &str, origin: &str, remotes: &str) -> bool {
    stderr.contains(&format!("{origin}: no valid signatures for ")) && is_open3d_remote(origin, remotes)
}

#[cfg(test)]
mod remote_url_tests {
    use super::*;
    #[test]
    fn recognizes_custom_names_and_rejects_unrelated_urls() {
        assert!(is_open3d_remote("arttool-oci", "arttool-oci\toci+https://github.com/carlesllavata/Open3D\n"));
        assert!(is_open3d_remote("custom", "custom\toci+https://github.com/carlesllavata/Open3D/\n"));
        for url in [
            "oci+https://github.com/carlesllavata/Open3D.evil",
            "oci+https://github.com/carlesllavata/Open3D/other",
            "oci+http://github.com/carlesllavata/Open3D",
            "https://github.com/carlesllavata/Open3D/linux",
            "https://github.com/carlesllavata/Open3D/arttool-oci.flatpakrepo",
            "https://github.com/carlesllavata/Open3D",
        ] {
            assert!(!is_open3d_remote("custom", &format!("custom\t{url}")));
        }
        assert!(!is_open3d_remote("other", "custom\toci+https://github.com/carlesllavata/Open3D"));
    }

    #[test]
    fn only_an_arttool_origin_missing_its_signatures_gets_the_lookaside() {
        // Flatpak 1.18's error for a verified OCI remote with no lookaside.
        let stderr = "error: Failed to update io.github.carlesllavata.phototool: phototool-origin: no valid signatures for \
                      ghcr.io/carlesllavata/phototool@sha256:968d75c9d16d5f7d089775f14a3b418d72a4c70d86986811ee2dd818da956b92";
        let remotes = "phototool-origin\toci+https://github.com/carlesllavata/Open3D\nflathub\thttps://dl.flathub.org/repo/\n";
        assert!(needs_signature_lookaside(stderr, "phototool-origin", remotes));
        // Another remote, another origin's failure, or another error: leave the remote alone.
        assert!(!needs_signature_lookaside(stderr, "phototool-origin", "phototool-origin\thttps://example.org\n"));
        assert!(!needs_signature_lookaside(stderr, "flathub", remotes));
        assert!(!needs_signature_lookaside("error: Server returned status 404", "phototool-origin", remotes));
    }
}

/// A bundle origin permits replacement when its URL is empty or points at Open3D OCI.
/// Missing rows are unknown, so preserve remote ownership conservatively.
pub fn remote_manages_flatpak(origin: &str, remotes: &str) -> bool {
    if origin.ends_with("-origin") && is_open3d_remote(origin, remotes) {
        return false;
    }
    remotes
        .lines()
        .find_map(|line| {
            let mut fields = line.split('\t');
            if fields.next().map(str::trim) != Some(origin) {
                return None;
            }
            Some(fields.next().is_some_and(|url| !url.trim().is_empty()))
        })
        .unwrap_or(true)
}

#[cfg(test)]
mod remote_ownership_tests {
    use super::*;
    #[test]
    fn only_confirmed_empty_urls_allow_bundle_replacement() {
        assert!(!remote_manages_flatpak("app-origin", "app-origin\t\n"));
        assert!(remote_manages_flatpak("renamed", "renamed\thttps://example.org/repo\n"));
        assert!(remote_manages_flatpak("renamed", "renamed\toci+https://github.com/carlesllavata/Open3D\n"));
        assert!(!remote_manages_flatpak("app-origin", "app-origin\toci+https://github.com/carlesllavata/Open3D\n"));
        assert!(remote_manages_flatpak("missing", "other\t\n"));
        assert!(!remote_manages_flatpak("app-origin", "app-origin"));
    }
}
