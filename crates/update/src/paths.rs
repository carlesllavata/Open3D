// SPDX-License-Identifier: GPL-3.0-or-later
//! Where an installer keeps its own files: downloads, records, logs.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::app::App;

/// An installer's folders.
#[derive(Clone, Debug)]
pub struct Paths {
    /// Records of what was installed, and tarball installs.
    pub data: PathBuf,
    /// Saved release answers and in-progress downloads.
    pub cache: PathBuf,
}

impl Paths {
    /// Discovers paths for an app from a specific executable path.
    pub fn for_exe(app: &App, exe: &Path) -> Result<Self, String> {
        if let Some(parent) = exe.parent()
            && parent.join("portable.txt").is_file()
        {
            let data_dir_name = format!("{}Data", app.name.replace(' ', ""));
            return Ok(Self::under(&parent.join(data_dir_name)));
        }
        let dir = if cfg!(any(target_os = "macos", target_os = "windows")) { app.name } else { app.id };
        Self::per_user(dir)
    }

    /// The per-user folders named `dir` (`Open3D Launcher` on macOS and Windows,
    /// `open3d-launcher` elsewhere, by convention).
    pub fn per_user(dir: &str) -> Result<Self, String> {
        let data = crate::folders::data_local_dir().ok_or("no per-user data folder on this system")?.join(dir);
        let cache = crate::folders::cache_dir().ok_or("no per-user cache folder on this system")?.join(dir);
        Ok(Self { data, cache })
    }

    /// Everything under one folder (portable mode, tests).
    pub fn under(root: &std::path::Path) -> Self {
        Self { data: root.join("data"), cache: root.join("cache") }
    }

    pub fn downloads(&self) -> PathBuf {
        self.cache.join("downloads")
    }

    /// Log files, and the error of a failed self-update waiting for the next start.
    pub fn logs(&self) -> PathBuf {
        self.data.join("logs")
    }

    /// Where tarball installs (FreeBSD, Linux without AppImage) are unpacked: `<data>/apps/<app>`.
    pub fn apps(&self) -> PathBuf {
        self.data.join("apps")
    }
}

/// Writes `bytes` to `path` via a temporary file and a rename, so a crash never leaves half a file.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create {} ({e})", dir.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| format!("couldn't create a temporary file in {} ({e})", dir.display()))?;
    tmp.write_all(bytes).map_err(|e| format!("couldn't write {} ({e})", tmp.path().display()))?;
    tmp.as_file().sync_all().map_err(|e| format!("couldn't sync {} ({e})", tmp.path().display()))?;
    tmp.persist(path).map_err(|e| format!("couldn't replace {} ({e})", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_does_not_overwrite_an_unrelated_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("installed.json");
        let unrelated = path.with_extension("tmp");
        std::fs::write(&unrelated, b"another writer's data").unwrap();
        write_atomic(&path, b"new manifest").unwrap();
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"another writer's data");
        assert_eq!(std::fs::read(&path).unwrap(), b"new manifest");
    }

    #[test]
    fn atomic_write_replaces_a_complete_file_and_cleans_up_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("installed.json");
        std::fs::write(&path, b"old manifest").unwrap();
        write_atomic(&path, b"new manifest").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new manifest");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

        let blocked = dir.path().join("directory.json");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::write(blocked.join("keep.txt"), b"keep").unwrap();
        assert!(write_atomic(&blocked, b"replacement").is_err());
        assert_eq!(std::fs::read(blocked.join("keep.txt")).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
