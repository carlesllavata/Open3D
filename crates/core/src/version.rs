// SPDX-License-Identifier: GPL-3.0-or-later
//! Versions: release tags (`v0.5.0`), installed versions from many sources, and ordering.

use std::cmp::Ordering;

pub use semver::Version;

/// Reads a version from a tag or an installed-version string.
///
/// Accepts `v0.5.0`, `0.5.0`, `0.2.0-rc.1`, Debian's `0.2.0~rc.1-1`, and the two-part `0.5`
/// some platforms report. Build metadata is kept but never affects ordering.
pub fn parse(text: &str) -> Option<Version> {
    let t = text.trim();
    let t = t.strip_prefix(['v', 'V']).unwrap_or(t);
    if t.is_empty() || t.len() > 64 {
        return None;
    }
    // Debian/RPM spell pre-releases with `~` and append a package revision after `-`.
    let debianish = t.contains('~');
    let t = if debianish { t.split_once('-').map_or(t, |(v, _)| v).replace('~', "-") } else { t.to_string() };
    if let Ok(v) = Version::parse(&t) {
        return Some(v);
    }
    // `0.5` → `0.5.0`
    let (core, rest) = match t.find(['-', '+']) {
        Some(i) => t.split_at(i),
        None => (t.as_str(), ""),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() == 2 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) {
        return Version::parse(&format!("{core}.0{rest}")).ok();
    }
    None
}

/// A version that's a plain `X.Y.Z` with no pre-release suffix.
pub fn is_stable(v: &Version) -> bool {
    v.pre.is_empty()
}

/// Orders by precedence, ignoring build metadata.
pub fn compare(a: &Version, b: &Version) -> Ordering {
    a.cmp_precedence(b)
}

/// True when `latest` is newer than `installed`.
pub fn is_newer(latest: &Version, installed: &Version) -> bool {
    compare(latest, installed) == Ordering::Greater
}

/// Platforms that only record `X.Y.Z` (the MSI ProductVersion, `CFBundleShortVersionString`)
/// drop a pre-release suffix. A numeric-only installed version therefore counts as current when it
/// matches a pre-release's numbers, rather than nagging forever.
pub fn is_update(latest: &Version, installed: &Version) -> bool {
    if installed.pre.is_empty()
        && !latest.pre.is_empty()
        && (latest.major, latest.minor, latest.patch) == (installed.major, installed.minor, installed.patch)
    {
        return false;
    }
    is_newer(latest, installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        parse(s).unwrap()
    }

    #[test]
    fn orders_versions_and_spots_updates() {
        assert!(is_newer(&v("0.10.0"), &v("0.9.9")));
        assert!(is_newer(&v("0.2.0"), &v("0.2.0-rc.5")));
        assert!(!is_newer(&v("0.2.0+abc"), &v("0.2.0")));
        assert!(is_update(&v("0.5.0"), &v("0.4.1")));
        assert!(!is_update(&v("0.5.0"), &v("0.5.0")));
        assert!(!is_update(&v("0.5.0"), &v("0.6.0")));
        // An MSI that recorded 0.3.0 for 0.3.0-rc.2 is not out of date against 0.3.0-rc.2.
        assert!(!is_update(&v("0.3.0-rc.2"), &v("0.3.0")));
        assert!(is_stable(&v("1.0.0")) && !is_stable(&v("1.0.0-beta.1")));
    }
}
