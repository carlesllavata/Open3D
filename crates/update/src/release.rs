// SPDX-License-Identifier: GPL-3.0-or-later
//! GitHub releases: reading the API's answer and `SHA256SUMS.txt`.
//!
//! Everything here is untrusted input. Sizes are bounded by the caller, download links must point
//! at the app's own release downloads, and checksums must look like checksums.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::version::Version;

/// One downloadable file of a release.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    /// Bytes; 0 when unknown.
    pub size: u64,
    /// `https://github.com/<repo>/releases/download/<tag>/<name>`.
    pub url: String,
    /// Lowercase hex SHA-256, when GitHub or `SHA256SUMS.txt` told us.
    pub sha256: Option<String>,
}

/// A published release of one app.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    pub version: Option<Version>,
    /// `PhotoTool v0.5.0`
    pub name: String,
    /// Unix seconds; `None` when only the tag is known.
    pub published_at: Option<i64>,
    pub html_url: String,
    /// Release notes (Markdown), capped.
    pub notes: String,
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn version_label(&self) -> String {
        self.version.as_ref().map_or_else(|| self.tag.clone(), ToString::to_string)
    }

    /// Fills in checksums from a `SHA256SUMS.txt` for assets that don't have one.
    pub fn apply_sums(&mut self, sums: &[(String, String)]) {
        for asset in &mut self.assets {
            if asset.sha256.is_none() {
                asset.sha256 = sums.iter().find(|(_, n)| *n == asset.name).map(|(h, _)| h.clone());
            }
        }
    }

    pub fn sums_asset(&self) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == "SHA256SUMS.txt")
    }
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Reads `sha256sum` output: `<hex>  <name>` (or `<hex> *<name>`) per line, one entry per name.
/// A line repeated as is counts once; a name listed with two different hashes is dropped,
/// whichever line comes first, so the list vouches for neither (a file nothing else vouches for
/// isn't installed).
pub fn parse_sha256sums(text: &str) -> Vec<(String, String)> {
    let mut sums: Vec<(String, String)> = Vec::new();
    // Where each name is in `sums`: a 4 MiB list holds some 60,000 names, too many to search.
    let mut index: HashMap<&str, usize> = HashMap::new();
    let mut conflicting: HashSet<&str> = HashSet::new();
    for line in text.lines() {
        let Some((hash, rest)) = line.trim().split_once(char::is_whitespace) else { continue };
        let name = rest.trim_start().trim_start_matches('*');
        if !is_hex64(hash) || !valid_file_name(name) {
            continue;
        }
        let hash = hash.to_ascii_lowercase();
        match index.get(name) {
            None => {
                index.insert(name, sums.len());
                sums.push((hash, name.to_string()));
            }
            Some(&i) => {
                if sums.get(i).is_some_and(|(h, _)| *h != hash) && conflicting.insert(name) {
                    log::warn!("SHA256SUMS.txt lists {name} with different checksums; ignoring it");
                }
            }
        }
    }
    sums.retain(|(_, n)| !conflicting.contains(n.as_str()));
    sums
}

/// A bare file name we'd be willing to save: no paths, no control characters.
pub fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && !name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "8a5edab282632443219e051e4ade2d1d5bbc671c781051bf1437897cbdfea0f1";

    #[test]
    fn a_full_size_list_keeps_its_order_and_finds_a_conflict_across_it() {
        // About as many names as fit in the 4 MiB a list may be, and a conflict between the
        // first and last lines.
        let other = "b".repeat(64);
        let mut text: String = (0..57_000).map(|i| format!("{HASH}  f{i}\n")).collect();
        text.push_str(&format!("{other}  f0\n"));
        assert!(text.len() <= 4 << 20);
        let sums = parse_sha256sums(&text);
        assert_eq!(sums.len(), 56_999);
        assert!(sums.iter().enumerate().all(|(i, (h, n))| *h == HASH && *n == format!("f{}", i + 1)));
    }

    #[test]
    fn file_names_cannot_escape() {
        for bad in ["", "..", "a/b", "a\\b", ".hidden", "c:x", "x\u{0}"] {
            assert!(!valid_file_name(bad), "{bad:?}");
        }
        assert!(valid_file_name("phototool-0.5.0-linux-x86_64.AppImage"));
    }
}
