// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading the catalog and downloading packages, from the web or from local files.

use crate::catalog::Location;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .user_agent(concat!("Open3D-Launcher/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Reads the catalog. A catalog on raw.githubusercontent.com is read through the GitHub API instead: raw files
/// sit in a 5-minute CDN cache that ignores query strings, the API's cache lasts at most 60 seconds. If the API
/// fails (for example its limit of 60 requests an hour per address), the raw file is read as before.
pub fn read_catalog(loc: &Location) -> Result<String, String> {
    if let Location::Url(url) = loc
        && let Some(api) = github_api_url(url)
        && let Ok(text) = read_github_api(&api)
    {
        return Ok(text);
    }
    read_text(loc)
}

/// `https://raw.githubusercontent.com/<owner>/<repo>/<ref>/<path>` →
/// `https://api.github.com/repos/<owner>/<repo>/contents/<path>?ref=<ref>`.
pub fn github_api_url(raw: &str) -> Option<String> {
    let rest = raw.strip_prefix("https://raw.githubusercontent.com/")?;
    let rest = rest.split(['?', '#']).next()?;
    let mut parts = rest.splitn(4, '/');
    let (owner, repo, git_ref, path) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if [owner, repo, git_ref, path].iter().any(|p| p.is_empty()) {
        return None;
    }
    Some(format!(
        "https://api.github.com/repos/{owner}/{repo}/contents/{path}?ref={git_ref}"
    ))
}

fn read_github_api(url: &str) -> Result<String, String> {
    let mut resp = agent()
        .get(url)
        .header("Accept", "application/vnd.github.raw+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|e| format!("Could not download {url}: {e}"))?;
    resp.body_mut()
        .read_to_string()
        .map_err(|e| format!("Could not read {url}: {e}"))
}

pub fn read_text(loc: &Location) -> Result<String, String> {
    match loc {
        Location::Url(url) => {
            let mut resp = agent()
                .get(url)
                .call()
                .map_err(|e| format!("Could not download {url}: {e}"))?;
            resp.body_mut()
                .read_to_string()
                .map_err(|e| format!("Could not read {url}: {e}"))
        }
        Location::File(path) => {
            std::fs::read_to_string(path).map_err(|e| format!("Could not read {}: {e}", path.display()))
        }
    }
}

/// Downloads `loc` into `dest`, reporting `(bytes done, total if known)`, and returns the file's sha256 (hex).
pub fn download(
    loc: &Location,
    dest: &Path,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<String, String> {
    let (mut reader, total): (Box<dyn Read>, Option<u64>) = match loc {
        Location::Url(url) => {
            let resp = agent()
                .get(url)
                .call()
                .map_err(|e| format!("Could not download {url}: {e}"))?;
            let total = resp.body().content_length();
            (Box::new(resp.into_body().into_reader()), total)
        }
        Location::File(path) => {
            let f = File::open(path).map_err(|e| format!("Could not open {}: {e}", path.display()))?;
            let total = f.metadata().ok().map(|m| m.len());
            (Box::new(f), total)
        }
    };
    let mut out = File::create(dest).map_err(|e| format!("Could not write {}: {e}", dest.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("Download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])
            .map_err(|e| format!("Could not write {}: {e}", dest.display()))?;
        done += n as u64;
        progress(done, total);
    }
    out.flush()
        .map_err(|e| format!("Could not write {}: {e}", dest.display()))?;
    Ok(to_hex(hasher.finalize().as_ref()))
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_github_urls_map_to_the_contents_api() {
        assert_eq!(
            github_api_url("https://raw.githubusercontent.com/carlesllavata/Open3D/main/catalog.json")
                .as_deref(),
            Some("https://api.github.com/repos/carlesllavata/Open3D/contents/catalog.json?ref=main")
        );
        assert_eq!(
            github_api_url("https://raw.githubusercontent.com/o/r/dev/sub/dir/c.json?x=1").as_deref(),
            Some("https://api.github.com/repos/o/r/contents/sub/dir/c.json?ref=dev")
        );
        assert_eq!(github_api_url("https://example.org/catalog.json"), None);
        assert_eq!(github_api_url("https://raw.githubusercontent.com/o/r/main"), None);
    }

    /// Maintainers, right after publishing: `cargo test -- --ignored`. The public catalog, read the way the
    /// launcher reads it, must already match the committed catalog.json.
    #[test]
    #[ignore = "needs the network; run after publishing"]
    fn live_catalog_matches_the_committed_one() {
        let local = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/catalog.json")).unwrap();
        let live = read_catalog(&Location::parse(crate::catalog::DEFAULT_CATALOG)).unwrap();
        let (a, b) = (
            crate::catalog::parse(&local).unwrap(),
            crate::catalog::parse(&live).unwrap(),
        );
        let versions = |c: &crate::catalog::Catalog| {
            let mut v: Vec<String> = c
                .apps
                .iter()
                .map(|a| format!("{} {}", a.id, a.latest.version))
                .collect();
            v.push(format!(
                "launcher {}",
                c.launcher.as_ref().map_or("-", |l| l.version.as_str())
            ));
            v
        };
        assert_eq!(versions(&b), versions(&a));
    }

    #[test]
    fn local_download_copies_and_hashes() {
        let dir = std::env::temp_dir().join(format!("open3d-net-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.bin");
        std::fs::write(&src, b"abc").unwrap();
        let dest = dir.join("dest.bin");
        let mut last = 0;
        let hash = download(&Location::File(src), &dest, |d, _| last = d).unwrap();
        assert_eq!(
            hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(last, 3);
        assert_eq!(std::fs::read(&dest).unwrap(), b"abc");
        std::fs::remove_dir_all(&dir).ok();
    }
}
