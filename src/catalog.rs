// SPDX-License-Identifier: GPL-3.0-or-later
//! The catalog: which apps exist, their latest version and where to download them.

use serde::Deserialize;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// Where the public catalog lives once the public repo exists.
pub const DEFAULT_CATALOG: &str = "https://raw.githubusercontent.com/carlesllavata/Open3D/main/catalog.json";

/// Highest catalog schema this launcher understands.
pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
pub struct Catalog {
    pub schema: u32,
    #[serde(default)]
    pub launcher: Option<Release>,
    #[serde(default)]
    pub apps: Vec<AppEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppEntry {
    /// Permanent identifier (`opentools`); the display name may change.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub summary: String,
    /// Folder name inside ApplicationPlugins, e.g. `OpenTools.bundle`.
    pub bundle: String,
    #[serde(default)]
    pub max_min: Option<u32>,
    pub latest: Release,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub version: String,
    #[serde(default)]
    pub date: String,
    /// Absolute http(s) URL, or a path relative to the catalog.
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub notes: String,
}

/// A catalog or package location: a web address or a local file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Url(String),
    File(PathBuf),
}

impl Location {
    pub fn parse(text: &str) -> Location {
        let t = text.trim();
        if t.starts_with("https://") || t.starts_with("http://") {
            Location::Url(t.to_string())
        } else {
            Location::File(PathBuf::from(t))
        }
    }

    /// Resolves `reference` (absolute URL or relative path) against this catalog location.
    pub fn join(&self, reference: &str) -> Location {
        let r = reference.trim();
        if r.starts_with("https://") || r.starts_with("http://") {
            return Location::Url(r.to_string());
        }
        match self {
            Location::Url(base) => {
                let dir = match base.rfind('/') {
                    Some(i) => &base[..=i],
                    None => base.as_str(),
                };
                Location::Url(format!("{dir}{}", r.trim_start_matches("./")))
            }
            Location::File(path) => {
                let p = Path::new(r);
                if p.is_absolute() {
                    Location::File(p.to_path_buf())
                } else {
                    let dir = path.parent().unwrap_or_else(|| Path::new("."));
                    Location::File(dir.join(p))
                }
            }
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Location::Url(u) => u.clone(),
            Location::File(p) => p.display().to_string(),
        }
    }
}

/// Parses and checks a catalog. Anything that would later be used as a path is validated here.
pub fn parse(text: &str) -> Result<Catalog, String> {
    let catalog: Catalog =
        serde_json::from_str(text).map_err(|e| format!("The catalog is not valid: {e}"))?;
    if catalog.schema > SCHEMA {
        return Err(format!(
            "The catalog uses format {} but this launcher only knows format {SCHEMA}. Update the launcher.",
            catalog.schema
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for app in &catalog.apps {
        if app.id.is_empty()
            || !app
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!("The catalog has an app with an invalid id: {:?}", app.id));
        }
        if !seen.insert(app.id.to_ascii_lowercase()) {
            return Err(format!("The catalog lists the app {:?} twice", app.id));
        }
        if !valid_bundle_name(&app.bundle) {
            return Err(format!(
                "The catalog has an invalid bundle name for {}: {:?}",
                app.id, app.bundle
            ));
        }
        check_release(&app.latest).map_err(|e| format!("{}: {e}", app.id))?;
    }
    if let Some(l) = &catalog.launcher {
        check_release(l).map_err(|e| format!("launcher: {e}"))?;
    }
    Ok(catalog)
}

fn check_release(r: &Release) -> Result<(), String> {
    if parse_version(&r.version).is_none() {
        return Err(format!("invalid version {:?}", r.version));
    }
    if r.sha256.len() != 64 || !r.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("invalid sha256".to_string());
    }
    if r.url.trim().is_empty() {
        return Err("missing download url".to_string());
    }
    Ok(())
}

/// A bundle name must be a single folder name ending in `.bundle`: no separators, no `..`.
pub fn valid_bundle_name(name: &str) -> bool {
    name.len() > ".bundle".len()
        && name.to_ascii_lowercase().ends_with(".bundle")
        && !name.starts_with('.')
        && !name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
        && !name.contains("..")
}

/// `0.5.0` → `[0, 5, 0]`. Only plain dotted numbers are versions.
pub fn parse_version(v: &str) -> Option<Vec<u64>> {
    let t = v.trim().trim_start_matches('v');
    if t.is_empty() {
        return None;
    }
    t.split('.').map(|p| p.parse::<u64>().ok()).collect()
}

/// Compares two versions; `1.2` equals `1.2.0`. Unparsable versions sort first.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    match (parse_version(a), parse_version(b)) {
        (Some(x), Some(y)) => {
            let n = x.len().max(y.len());
            for i in 0..n {
                let (p, q) = (x.get(i).copied().unwrap_or(0), y.get(i).copied().unwrap_or(0));
                if p != q {
                    return p.cmp(&q);
                }
            }
            Ordering::Equal
        }
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "38027d51d69a5d05493627a0634dbea52d6cd793c8f072d2373d3f4163eec7db";

    fn catalog_with_bundle(bundle: &str) -> String {
        format!(
            r#"{{"schema":1,"apps":[{{"id":"opentools","name":"OpenTools","bundle":"{bundle}",
            "latest":{{"version":"0.5.0","url":"OpenTools-0.5.0.zip","sha256":"{HASH}"}}}}]}}"#
        )
    }

    #[test]
    fn parses_a_valid_catalog() {
        let c = parse(&catalog_with_bundle("OpenTools.bundle")).unwrap();
        assert_eq!(c.apps[0].latest.version, "0.5.0");
    }

    #[test]
    fn rejects_bundle_names_that_escape_the_folder() {
        for bad in [
            "../OpenTools.bundle",
            "a\\\\b.bundle",
            "OpenTools",
            ".bundle",
            "..bundle",
            "C:x.bundle",
        ] {
            assert!(
                parse(&catalog_with_bundle(bad)).is_err(),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_newer_schema() {
        assert!(
            parse(r#"{"schema":2,"apps":[]}"#)
                .unwrap_err()
                .contains("Update the launcher")
        );
    }

    #[test]
    fn compares_versions_numerically() {
        assert_eq!(compare_versions("0.10.0", "0.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("1.2", "1.2.0"), Ordering::Equal);
        assert_eq!(compare_versions("v0.5.0", "0.5.0"), Ordering::Equal);
        assert_eq!(compare_versions("0.0.0", "0.5.0"), Ordering::Less);
    }

    #[test]
    fn joins_relative_package_locations() {
        let web = Location::parse("https://example.org/a/catalog.json");
        assert_eq!(
            web.join("x.zip"),
            Location::Url("https://example.org/a/x.zip".into())
        );
        assert_eq!(
            web.join("https://cdn.org/y.zip"),
            Location::Url("https://cdn.org/y.zip".into())
        );
        let file = Location::parse(r"C:\cat\catalog.json");
        assert_eq!(
            file.join("pkg/x.zip"),
            Location::File(PathBuf::from(r"C:\cat").join("pkg/x.zip"))
        );
    }
}
