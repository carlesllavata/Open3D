// SPDX-License-Identifier: GPL-3.0-or-later
//! Portable Windows packages: bounded extraction and replacement of recorded files only.

use std::path::{Component, Path, PathBuf};

const MAX_FILES: usize = 10_000;
const MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;

fn extract(file: &Path, destination: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
    let file = std::fs::File::open(file).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("couldn't read the portable ZIP ({e})"))?;
    if archive.len() > MAX_FILES {
        return Err("The portable ZIP contains too many files.".into());
    }
    let mut files = Vec::new();
    let mut entries = Vec::new();
    let mut total = 0u64;
    let mut seen = std::collections::BTreeSet::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|e| e.to_string())?;
        let relative = entry.enclosed_name().ok_or("The portable ZIP contains an unsafe path.")?;
        if relative.components().any(|c| !matches!(c, Component::Normal(_)))
            || entry.name().map_or(true, |name| name.contains(['\\', ':']))
            || entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("The portable ZIP contains an unsafe path or link.".into());
        }
        // Windows treats case variants as the same file; reject duplicate and colliding names.
        if !seen.insert(relative.to_string_lossy().to_lowercase()) {
            return Err("The portable ZIP contains duplicate paths.".into());
        }
        let target = destination.join(&relative);
        entries.push(relative.clone());
        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            continue;
        }
        total = total.checked_add(entry.size()).filter(|size| *size <= MAX_BYTES).ok_or("The portable ZIP is too large.")?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(&target).map_err(|e| e.to_string())?;
        use std::io::Read;
        let declared = entry.size();
        let copied = std::io::copy(&mut (&mut entry).take(declared.saturating_add(1)), &mut output).map_err(|e| e.to_string())?;
        if copied != declared {
            return Err("The portable ZIP has an unexpected unpacked size.".into());
        }
        files.push(relative);
    }
    Ok((files, entries))
}

/// Unpacks the launcher's own portable zip into `staging`, with the same checks as an app's, and
/// copies it over `folder` for a self-update. `check` must accept every unpacked file before
/// any is copied, and again before the backup is removed. The archive must include `executable`.
/// When every file sits in one top-level folder, that folder's contents are the program. A copy
/// that fails puts the previous files back. When one of them can't be put back,
/// the previous files move to a `<staging>-recovery-<time>` folder beside `staging`, which no
/// later update removes, and the error names it. `staging` is removed afterwards.
pub fn unpack_over(
    zip: &Path,
    staging: &Path,
    folder: &Path,
    executable: &std::ffi::OsStr,
    check: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let backup = staging.join("backup");
    // A failed update whose files couldn't be moved to a recovery folder: never clear them.
    if backup.exists() {
        return Err(format!("the previous files of a failed update are in {}; move them out of the way first", backup.display()));
    }
    let _ = std::fs::remove_dir_all(staging);
    let mut keep = false;
    let result = replace_with(zip, staging, folder, executable, check, &mut keep);
    if !keep {
        let _ = std::fs::remove_dir_all(staging);
    }
    result
}

fn replace_with(
    zip: &Path,
    staging: &Path,
    folder: &Path,
    executable: &std::ffi::OsStr,
    check: &dyn Fn(&Path) -> Result<(), String>,
    keep: &mut bool,
) -> Result<(), String> {
    let unpacked = staging.join("unpacked");
    let backup = staging.join("backup");
    std::fs::create_dir_all(&unpacked).map_err(|e| e.to_string())?;
    let (files, _) = extract(zip, &unpacked)?;
    let top = single_top_folder(&files);
    let files = files
        .into_iter()
        .map(|relative| {
            let inside = match &top {
                Some(top) => relative.strip_prefix(top).map_err(|e| e.to_string())?.to_path_buf(),
                None => relative.clone(),
            };
            Ok((relative, inside))
        })
        .collect::<Result<Vec<_>, String>>()?;
    if !files.iter().any(|(_, inside)| inside == Path::new(executable)) {
        return Err(format!("the update doesn't contain {}", executable.to_string_lossy()));
    }
    for (relative, _) in &files {
        check(&unpacked.join(relative))?;
    }
    let mut saved = Vec::new();
    let mut created = Vec::new();
    let copied = (|| {
        for (relative, inside) in &files {
            let target = folder.join(inside);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            if target.is_file() {
                let previous = backup.join(inside);
                std::fs::create_dir_all(previous.parent().ok_or("no backup folder")?).map_err(|e| e.to_string())?;
                std::fs::copy(&target, &previous).map_err(|e| e.to_string())?;
                saved.push(inside.to_path_buf());
            } else {
                created.push(target.clone());
            }
            std::fs::copy(unpacked.join(relative), &target).map_err(|e| format!("couldn't replace {} ({e})", target.display()))?;
        }
        // Keep the previous files until the installed copies have passed the same checks.
        for (_, inside) in &files {
            check(&folder.join(inside))?;
        }
        Ok::<(), String>(())
    })();
    if let Err(error) = copied {
        for target in &created {
            let _ = std::fs::remove_file(target);
        }
        for inside in &saved {
            if let Err(restore) = std::fs::copy(backup.join(inside), folder.join(inside)) {
                let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                let mut recovery = staging.as_os_str().to_owned();
                recovery.push(format!("-recovery-{stamp}"));
                let recovery = PathBuf::from(recovery);
                let kept = if std::fs::rename(&backup, &recovery).is_ok() {
                    recovery
                } else {
                    *keep = true;
                    backup.clone()
                };
                return Err(format!(
                    "{error}; couldn't put back {} ({restore}). The previous files are in {}",
                    folder.join(inside).display(),
                    kept.display()
                ));
            }
        }
        return Err(error);
    }
    Ok(())
}

/// The one folder that holds every file, when there is one.
fn single_top_folder(files: &[PathBuf]) -> Option<PathBuf> {
    let first = files.first()?.components().next()?;
    files
        .iter()
        .all(|file| {
            let mut parts = file.components();
            parts.next() == Some(first) && parts.next().is_some()
        })
        .then(|| PathBuf::from(first.as_os_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Previous files left in the staging folder by a failed update are never cleared by the next one.
    #[test]
    fn a_self_update_keeps_the_previous_files_of_a_failed_one() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("launcher-update");
        std::fs::create_dir_all(staging.join("backup")).unwrap();
        std::fs::write(staging.join("backup").join("open3d-launcher.exe"), "0.1.0").unwrap();
        let zip = archive(dir.path(), "update.zip", &[("open3d-launcher.exe", b"0.2.0")]);
        let folder = dir.path().join("Open3D Launcher");
        std::fs::create_dir_all(&folder).unwrap();
        let error = unpack_over(&zip, &staging, &folder, std::ffi::OsStr::new("open3d-launcher.exe"), &|_| Ok(())).unwrap_err();
        assert!(error.contains("previous files"), "{error}");
        assert_eq!(std::fs::read_to_string(staging.join("backup").join("open3d-launcher.exe")).unwrap(), "0.1.0");
        assert!(!folder.join("open3d-launcher.exe").exists());
    }

    /// Validation of the installed copy is part of the transaction, while a backup still exists.
    #[test]
    fn a_self_update_that_fails_validation_puts_the_previous_files_back() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("launcher-update");
        let folder = dir.path().join("Open3D Launcher");
        std::fs::create_dir_all(&folder).unwrap();
        let exe = folder.join("open3d-launcher.exe");
        std::fs::write(&exe, "old launcher").unwrap();
        let zip = archive(dir.path(), "update.zip", &[("launcher/open3d-launcher.exe", b"new launcher"), ("launcher/README.md", b"new readme")]);
        let checked = std::cell::Cell::new(false);
        let error = unpack_over(&zip, &staging, &folder, std::ffi::OsStr::new("open3d-launcher.exe"), &|file| {
            if file == exe {
                assert_eq!(std::fs::read(file).unwrap(), b"new launcher");
                assert!(folder.join("README.md").is_file());
                checked.set(true);
                Err("installed launcher failed validation".into())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error, "installed launcher failed validation");
        assert!(checked.get());
        assert_eq!(std::fs::read(&exe).unwrap(), b"old launcher");
        assert!(!folder.join("README.md").exists());
        assert!(!staging.exists());
    }

    fn archive(root: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let path = root.join(name);
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for (name, content) in files {
            if name.ends_with('/') {
                zip.add_directory(*name, zip::write::SimpleFileOptions::default()).unwrap();
            } else {
                zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
                zip.write_all(content).unwrap();
            }
        }
        zip.finish().unwrap();
        path
    }

    /// ZIP 9 strips a drive prefix from enclosed paths; the original name must still be rejected.
    #[test]
    fn rejects_drive_prefixed_names_before_writing_files() {
        let dir = tempfile::tempdir().unwrap();
        let zip = archive(dir.path(), "drive.zip", &[("c:escape.exe", b"bad".as_slice())]);
        let output = dir.path().join("output");
        std::fs::create_dir(&output).unwrap();
        assert!(extract(&zip, &output).is_err());
        assert_eq!(std::fs::read_dir(&output).unwrap().count(), 0);
    }
}
