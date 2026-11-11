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

/// Orders by precedence, ignoring build metadata.
pub fn compare(a: &Version, b: &Version) -> Ordering {
    a.cmp_precedence(b)
}

/// True when `latest` is newer than `installed`.
pub fn is_newer(latest: &Version, installed: &Version) -> bool {
    compare(latest, installed) == Ordering::Greater
}

/// The pre-release labels the suite publishes, which sort against releases as semver says
/// (`0.17.0-rc.2` is before `0.17.0`).
const RELEASE_CHANNELS: [&str; 4] = ["alpha", "beta", "rc", "nightly"];

/// Whether `v` is a build made from source rather than a published release: it has a pre-release
/// label the suite never publishes (`0.17.0-integration.20261029.f5b3f123`, `0.18.0-dev`).
pub fn is_local_build(v: &Version) -> bool {
    let label = v.pre.as_str().split('.').next().unwrap_or_default();
    !v.pre.is_empty() && !RELEASE_CHANNELS.iter().any(|channel| label.eq_ignore_ascii_case(channel))
}

/// Whether a copy running `running` should be offered `latest`. As [`is_newer`], except for a
/// local build: semver puts `0.17.0-integration…` before `0.17.0`, but a build from `main` already
/// holds 0.17.0's changes and more, so replacing it with 0.17.0 is a downgrade. A local build is
/// only offered a strictly higher `X.Y.Z`.
pub fn offers_update(latest: &Version, running: &Version) -> bool {
    if is_local_build(running) {
        return (latest.major, latest.minor, latest.patch) > (running.major, running.minor, running.patch);
    }
    is_newer(latest, running)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        parse(s).unwrap()
    }

    #[test]
    fn a_build_from_main_is_never_offered_its_own_release() {
        let running = v("0.17.0-integration.20261029.f5b3f123");
        assert!(is_local_build(&running));
        assert!(!offers_update(&v("0.17.0"), &running));
        assert!(!offers_update(&v("0.16.4"), &running));
        assert!(!offers_update(&v("0.17.0-rc.3"), &running));
        assert!(offers_update(&v("0.17.1"), &running));
        assert!(offers_update(&v("0.18.0"), &running));
        assert!(offers_update(&v("1.0.0"), &v("0.18.0-dev")));
        assert!(!offers_update(&v("0.18.0"), &v("0.18.0-DEV.4")));
    }

    #[test]
    fn published_pre_releases_and_releases_order_as_before() {
        for label in ["0.17.0-rc.2", "0.17.0-beta.1", "0.17.0-alpha", "0.17.0-nightly.20261029", "0.17.0-RC.1"] {
            let running = v(label);
            assert!(!is_local_build(&running), "{label}");
            assert!(offers_update(&v("0.17.0"), &running), "{label}");
        }
        assert!(offers_update(&v("0.17.0-rc.3"), &v("0.17.0-rc.2")));
        assert!(!offers_update(&v("0.17.0-rc.2"), &v("0.17.0-rc.2")));
        assert!(!is_local_build(&v("0.17.0")));
        assert!(offers_update(&v("0.17.1"), &v("0.17.0")));
        assert!(!offers_update(&v("0.17.0"), &v("0.17.0")));
        assert!(!offers_update(&v("0.16.9"), &v("0.17.0")));
        assert!(!offers_update(&v("0.17.0+build.5"), &v("0.17.0")));
    }
}
