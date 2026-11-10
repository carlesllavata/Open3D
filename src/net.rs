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
