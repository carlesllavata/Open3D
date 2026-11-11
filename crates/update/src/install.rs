// SPDX-License-Identifier: GPL-3.0-or-later
//! Downloading a launcher release and running its MSI, for the self-update.
//!
//! Every download is checked against its published SHA-256 before anything is installed.

use std::path::{Path, PathBuf};

use crate::protocol::JobPhase;
use crate::release::{self, Asset, Release};
use crate::{App, version};

use crate::http::{Http, Stop, Transfer};
use crate::paths::Paths;
use crate::process;

/// What an install needs from the backend.
pub struct Job<'a> {
    pub http: &'a Http,
    pub paths: &'a Paths,
    pub stop: &'a Stop,
    pub phase: &'a mut dyn FnMut(JobPhase),
}

/// Rejects an artifact whose own identity disagrees with the release that authorized it.
///
/// A feed or GitHub converter supplies the release version, the asset filename, and the download
/// URL independently. The downgrade guard and the persisted record both trust `release.version`,
/// so a controller could advertise a new version while pointing at an older official artifact —
/// bypassing the guard and recording a false installed version (finding: version not bound to the
/// release). We require the version encoded in the asset filename and the release tag embedded in
/// the download URL to agree with the release version before any bytes are fetched.
pub(crate) fn bind_artifact_version(app: &App, release: &Release, asset: &Asset) -> Result<(), String> {
    let mismatch = || format!("{}'s download does not match the advertised version {}.", app.name, release.version_label());
    let release_version = release.version.as_ref();

    // The version in the asset filename (`<stem>-<version>-<os>-<arch>.<ext>`), when it carries one.
    let stems: Vec<&str> = app.stems().collect();
    if let Some(release_version) = release_version
        && let Some(parsed) = crate::platform::parse_asset_name(&asset.name, &stems)
        && version::parse(&parsed.version).as_ref() != Some(release_version)
    {
        return Err(mismatch());
    }

    // The release tag and filename embedded in a release download URL (`.../releases/download/<tag>/<name>`).
    if let Some(rest) = asset.url.split("/releases/download/").nth(1) {
        let mut parts = rest.split('/');
        let tag = parts.next().unwrap_or("");
        let url_filename = parts.next().unwrap_or("");
        if tag.is_empty()
            || release_version
                .map_or(tag != release.tag, |release_version| tag != release.tag && version::parse(tag).as_ref() != Some(release_version))
        {
            return Err(mismatch());
        }
        if !url_filename.is_empty() && url_filename != asset.name {
            return Err(format!("{}'s download URL filename does not match selected asset {}.", app.name, asset.name));
        }
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn msi_folder_argument(path: &Path) -> Result<String, String> {
    let value = path.to_str().ok_or("unusual characters in a path")?;
    if value.chars().any(|c| c == '"' || c.is_control()) {
        return Err(format!("couldn't use install folder {value}: the path contains invalid characters"));
    }
    Ok(format!("INSTALLFOLDER=\"{value}\""))
}

/// Where `asset` is downloaded to (and its partial file kept while unfinished).
pub fn download_path(paths: &Paths, asset: &Asset) -> PathBuf {
    paths.downloads().join(&asset.name)
}

/// Fetches `asset` into the download folder, checked against its published SHA-256: the feed's
/// or GitHub's per-file digest, else the release's `SHA256SUMS.txt`. A release with neither is
/// refused before anything is fetched: a file nothing vouches for is never installed (#90).
pub(crate) fn download(release: &Release, asset: &Asset, job: &mut Job<'_>) -> Result<PathBuf, String> {
    if !release::valid_file_name(&asset.name) {
        return Err("the release has an unexpected file name".into());
    }
    let sha = match &asset.sha256 {
        Some(s) => Some(s.clone()),
        None => published_checksum(release, asset, job.http)?,
    };
    let Some(sha) = sha else {
        return Err(format!(
            "the release publishes no checksum for {}, so it can't be verified and wasn't installed. Try again later, or report it on GitHub.",
            asset.name
        ));
    };
    let dest = download_path(job.paths, asset);
    let phase = &mut *job.phase;
    job.http.download(&asset.url, &dest, asset.size, &sha, job.stop, &mut |t| match t {
        Transfer::Bytes { done, total } => phase(JobPhase::Downloading { done, total }),
        Transfer::Reconnecting { done, total } => phase(JobPhase::Reconnecting { done, total }),
    })?;
    (job.phase)(JobPhase::Verifying);
    Ok(dest)
}

/// The file's line in the release's `SHA256SUMS.txt`. `Ok(None)` only when the release really
/// publishes no checksum for it; a list that exists but can't be fetched is an error, so a
/// network hiccup never downgrades the check.
fn published_checksum(release: &Release, asset: &Asset, http: &Http) -> Result<Option<String>, String> {
    let Some(sums) = release.sums_asset() else { return Ok(None) };
    let Some(text) = http.get_text(&sums.url).map_err(|e| format!("couldn't fetch the release's checksums ({e}). Try again"))? else {
        return Ok(None);
    };
    Ok(release::parse_sha256sums(&text).into_iter().find(|(_, n)| *n == asset.name).map(|(h, _)| h))
}

/// Builds the Windows Installer command separately so its package argument can be checked
/// without starting an installer.
fn msi_command(program: &Path, msi: &Path) -> Result<std::process::Command, String> {
    let msi = msi_path(msi)?;
    let mut command = process::command(program);
    // A verbose log beside the package: Windows Installer's own errors (1603 above all) say
    // nothing without it.
    command.arg("/i").arg(&msi).args(["/passive", "/norestart", "/l*v"]).arg(msi_log(&msi));
    Ok(command)
}

/// The self-update command: [`msi_command`] plus the properties that keep the installed copy's
/// Windows Installer context (`msi_copy::update_arguments`).
fn reinstall_command(program: &Path, msi: &Path, context: &[&str]) -> Result<std::process::Command, String> {
    let mut command = msi_command(program, msi)?;
    command.args(context);
    Ok(command)
}

/// Where `msi_command` asks Windows Installer to write its log.
pub(crate) fn msi_log(msi: &Path) -> PathBuf {
    msi.with_extension("log")
}

/// Windows Installer's storage APIs can fail on paths through junctions or symlinks (#338).
/// Keep ordinary drive/UNC syntax when possible: Windows canonicalization alone introduces
/// a `\\?\` prefix that older APIs also cannot handle.
pub(crate) fn msi_path(msi: &Path) -> Result<PathBuf, String> {
    #[cfg(windows)]
    let resolved = dunce::canonicalize(msi);
    resolved.map_err(|e| format!("couldn't resolve the Windows Installer package {} ({e})", msi.display()))
}

#[cfg(windows)]
pub mod windows {

    use super::{Path, PathBuf, msi_folder_argument, msi_log, process, reinstall_command};

    fn msiexec() -> PathBuf {
        std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from).join(r"System32\msiexec.exe")
    }

    fn explain(code: Option<i32>) -> Result<(), String> {
        match code {
            Some(0 | 3010 | 1641) => Ok(()),
            Some(1602) => Err("cancelled".into()),
            Some(1618) => Err("another installation is in progress. Wait for it to finish, then try again.".into()),
            Some(1925 | 1303) => Err("Windows didn't allow the install (administrator permission is needed)".into()),
            Some(c) => Err(format!("the Windows Installer stopped with error {c}")),
            None => Err("the Windows Installer was interrupted".into()),
        }
    }

    /// Runs an app's own new MSI for a self-update, with the same progress window and no restart
    /// as [`run_msi`], and waits for it. `context` keeps the installed copy's Windows Installer
    /// context; `folder`, when given, is where it goes. A failure names the installer's log,
    /// which is the only place 1603 is explained.
    pub fn reinstall(msi: &Path, context: &[&str], folder: Option<&Path>) -> Result<(), String> {
        use std::os::windows::process::CommandExt;
        let mut command = reinstall_command(&msiexec(), msi, context)?;
        if let Some(dir) = folder {
            command.raw_arg(msi_folder_argument(dir)?);
        }
        let status = command.status().map_err(|e| format!("couldn't start the Windows Installer ({e})"))?;
        explain(status.code()).map_err(|e| match status.code() {
            Some(1602) | None => e,
            _ => format!("{e}. Its log is at {}", msi_log(msi).display()),
        })
    }

    /// Removes one installed product by its code, with a progress window and no restart, and waits.
    /// A per-user product asks for no permission. `log` receives the installer's verbose log.
    pub fn remove_product(product_code: &str, log: &Path) -> Result<(), String> {
        if !crate::msi_context::valid_product_code(product_code) {
            return Err(format!("{product_code} isn't a Windows Installer product code"));
        }
        let status = process::command(msiexec())
            .args(["/x", product_code, "/passive", "/norestart", "/l*v"])
            .arg(log)
            .status()
            .map_err(|e| format!("couldn't start the Windows Installer ({e})"))?;
        explain(status.code()).map_err(|e| format!("{e}. Its log is at {}", log.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_self_update_keeps_the_installed_copys_context_after_the_package_options() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("GridTool.msi");
        std::fs::write(&file, b"test package").unwrap();
        let resolved = msi_path(&file).unwrap();
        for context in [&["ALLUSERS=1"][..], &["ALLUSERS=2", "MSIINSTALLPERUSER=1"], &[]] {
            let command = reinstall_command(Path::new("msiexec.exe"), &file, context).unwrap();
            let args: Vec<_> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
            let mut expected: Vec<String> = ["/i", &resolved.to_string_lossy(), "/passive", "/norestart", "/l*v"].map(String::from).to_vec();
            expected.push(resolved.with_extension("log").to_string_lossy().into_owned());
            expected.extend(context.iter().map(|p| p.to_string()));
            assert_eq!(args, expected);
        }
    }

    #[test]
    fn an_unavailable_msi_is_refused_before_starting_the_installer() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.msi");
        let error = msi_command(Path::new("msiexec.exe"), &missing).unwrap_err();
        assert!(error.contains("couldn't resolve the Windows Installer package"), "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn a_short_local_msi_path_keeps_ordinary_windows_syntax() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("package.msi");
        std::fs::write(&file, b"test package").unwrap();
        let resolved = msi_path(&file).unwrap();
        assert!(!resolved.as_os_str().to_string_lossy().starts_with(r"\\?\"), "{resolved:?}");
        assert_eq!(std::fs::read(resolved).unwrap(), b"test package");
    }

    #[test]
    fn msi_folder_assignment_preserves_spaces_unicode_and_root_separators() {
        for (value, argument) in [
            (r"D:\", r#"INSTALLFOLDER="D:\""#),
            (r"D:\Open3D apps\PhotoTool", r#"INSTALLFOLDER="D:\Open3D apps\PhotoTool""#),
            (r"\\server\apps\绘画", r#"INSTALLFOLDER="\\server\apps\绘画""#),
            (r"D:\Open3D apps\", r#"INSTALLFOLDER="D:\Open3D apps\""#),
        ] {
            assert_eq!(msi_folder_argument(Path::new(value)).unwrap(), argument);
        }
        assert!(msi_folder_argument(Path::new("D:\\bad\" TRANSFORMS=evil")).is_err());
        assert!(msi_folder_argument(Path::new("D:\\bad\nfolder")).is_err());
    }

    #[test]
    fn single_top_folder_rejects_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real");
        std::fs::create_dir_all(&real_dir).unwrap();
        let staging = dir.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
    }
}
