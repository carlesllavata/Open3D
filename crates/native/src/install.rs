// SPDX-License-Identifier: GPL-3.0-or-later
//! Installing, updating, removing and starting Open3D apps.
//!
//! Apps go where their platform keeps applications, installed the platform's own way:
//!
//! - macOS: the signed `.app` from the release DMG into `/Applications`.
//! - Windows: the release MSI (Windows Installer), for the current user into
//!   `%LOCALAPPDATA%\Programs\Open3D\<App>` by default. An update keeps the installed copy's folder
//!   and scope (per-machine copies stay in `C:\Program Files\Open3D\<App>`).
//! - Linux: the distro's own package (`.deb` through apt on the Debian family, `.rpm` through
//!   dnf/zypper/rpm on the Fedora, Red Hat and SUSE families, read from `os-release`) with the
//!   system's polkit password prompt; otherwise a Flatpak, otherwise an AppImage in
//!   `~/Applications` (the AppImage convention), with a menu entry and icon in
//!   `~/.local/share` so it shows up like any other app. A Flatpak or AppImage launcher prefers
//!   its own format.
//! - FreeBSD: the release's `/usr/local` tree, with a polkit prompt (or `~/.local` without one).
//! - Linux, inside the Flatpak launcher with no usable host Flatpak (none installed, or
//!   `flatpak-spawn` not allowed): the release tarball, unpacked into the launcher's own folder
//!   (`<data>/apps/<app>`) and run inside the same sandbox with `HOME` pointed at a `home/`
//!   folder beside it, so the app's settings stay in the sandbox too.
//!
//! Every download is checked against its published SHA-256 before anything is unpacked, a
//! running app is never replaced, and a new copy is fully in place before the old one goes.

use std::path::{Path, PathBuf};

use open3d_launcher_core::Arch;
use open3d_launcher_core::protocol::{CANCELLED, INSTALL_CANCELLED, InstallLocations, JobPhase};
use open3d_launcher_core::release::{self, Asset, Release};
use open3d_launcher_core::{InstallSource, Installed, PackageKind, SuiteApp, version};

use crate::http::{Http, Stop, Transfer};
use crate::manifest::Record;
use crate::paths::Paths;
use crate::process;

/// What an install needs from the backend.
pub(crate) struct Job<'a> {
    pub http: &'a Http,
    pub paths: &'a Paths,
    pub stop: &'a Stop,
    pub phase: &'a mut dyn FnMut(JobPhase),
    pub install_locations: &'a InstallLocations,
    /// Write over whatever [`occupied`] found.
    pub replace: bool,
    /// Set only when the person chose this older version by name over a newer installed copy.
    pub downgrade: Option<Downgrade>,
}

/// How a downgrade the person chose is undone if it fails: the installed copy stays until the
/// older one is in place.
#[derive(Clone, Copy)]
pub(crate) enum Downgrade {
    KeepOldCopy,
}

type RecordCommit<'a> = Option<&'a mut dyn FnMut(&Record) -> Result<(), String>>;

#[allow(clippy::too_many_arguments)]
pub(crate) fn install_at(
    app: &SuiteApp,
    release: &Release,
    asset: &Asset,
    kind: PackageKind,
    existing: Option<&Installed>,
    previous: Option<&Record>,
    job: &mut Job<'_>,
    commit: RecordCommit<'_>,
) -> Result<Option<Record>, String> {
    #[cfg(windows)]
    let _commit = commit;
    let _destination = destination(app, kind, existing, previous, job.install_locations)?;
    if let Some(inst) = existing
        && inst.source.launcher_can_update()
        && process::is_running(Path::new(&inst.location))
    {
        return Err(format!("{} is running. Quit it, then try again.", app.name));
    }
    if kind == PackageKind::MaxBundle && crate::max_bundle::max_running() {
        return Err(crate::max_bundle::max_running_error());
    }
    bind_artifact_version(app, release, asset)?;
    // A feed can't send an app back to an older build; only a version the person chose by name
    // (a downgrade with a way back if it fails) gets past this.
    if !job.replace
        && job.downgrade.is_none()
        && let Some(inst) = existing
        && let (Some(rel_v), Some(inst_v)) = (release.version.as_ref(), inst.version.as_ref())
        && version::is_newer(inst_v, rel_v)
    {
        return Err(format!("Cannot update {} to {}: installed version {} is newer", app.name, release.version_label(), inst_v));
    }
    log::info!("{}: starting {} to {} ({})", app.name, if existing.is_some() { "update" } else { "install" }, release.version_label(), kind.label());
    let (file, _sha256) = download(release, asset, job)?;
    log::info!("{}: download ready ({}), running installer...", app.name, file.display());
    // Past this point the install can't be interrupted halfway.
    (job.phase)(JobPhase::Installing);
    let version = release.version_label();
    let result = match kind {
        PackageKind::Msi | PackageKind::PortableZip => Err(format!("{} is a 3ds Max plug-in, not a {}", app.name, kind.label())),
        PackageKind::MaxBundle => crate::max_bundle::install(app, &file, &version).map(Some),
    };
    match &result {
        Ok(_) => log::info!("{}: successfully installed/updated to {}", app.name, release.version_label()),
        Err(e) if e == CANCELLED => log::info!("{}: installation cancelled", app.name),
        Err(e) => log::error!("{}: installation failed: {e}", app.name),
    }
    // A failed install keeps the download: the next try checks it against its published checksum
    // again and skips the transfer.
    if result.is_ok() {
        let _ = std::fs::remove_file(&file);
    }
    // The system's own prompt was dismissed: the download itself wasn't cancelled.
    result.map_err(|e| if e == CANCELLED { INSTALL_CANCELLED.to_string() } else { e })
}

/// Rejects an artifact whose own identity disagrees with the release that authorized it.
///
/// A feed or GitHub converter supplies the release version, the asset filename, and the download
/// URL independently. The downgrade guard and the persisted record both trust `release.version`,
/// so a controller could advertise a new version while pointing at an older official artifact —
/// bypassing the guard and recording a false installed version (finding: version not bound to the
/// release). We require the version encoded in the asset filename and the release tag embedded in
/// the download URL to agree with the release version before any bytes are fetched.
pub(crate) fn bind_artifact_version(app: &SuiteApp, release: &Release, asset: &Asset) -> Result<(), String> {
    let mismatch = || format!("{}'s download does not match the advertised version {}.", app.name, release.version_label());
    let release_version = release.version.as_ref();

    // The version in the asset filename (`<stem>-<version>-<os>-<arch>.<ext>`), when it carries one.
    let stems: Vec<&str> = app.stems().collect();
    if let Some(release_version) = release_version
        && let Some(parsed) = open3d_launcher_core::platform::parse_asset_name(&asset.name, &stems)
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

/// What a new install would write over: something already at its destination that the launcher
/// didn't put there.
pub(crate) fn occupied(
    app: &SuiteApp,
    _asset: &Asset,
    kind: PackageKind,
    existing: Option<&Installed>,
    previous: Option<&Record>,
    locations: &InstallLocations,
    paths: &Paths,
) -> Result<Option<PathBuf>, String> {
    if existing.is_some() || previous.is_some() {
        return Ok(None);
    }
    let folder = destination(app, kind, None, None, locations)?;
    let target = match kind {
        PackageKind::Msi => folder,
        PackageKind::PortableZip => Some(folder.unwrap_or_else(|| paths.apps()).join(app.id)),
        PackageKind::MaxBundle => crate::max_bundle::bundle_dir(app),
    };
    Ok(target.filter(|t| std::fs::symlink_metadata(t).is_ok()))
}

/// The folder a new copy goes in: beside the launcher's copy it replaces, else the user's choice,
/// else the platform default (`None`). macOS updates find their bundle's folder themselves.
fn destination(
    app: &SuiteApp,
    kind: PackageKind,
    existing: Option<&Installed>,
    previous: Option<&Record>,
    locations: &InstallLocations,
) -> Result<Option<PathBuf>, String> {
    if kind == PackageKind::MaxBundle {
        return Ok(None);
    }
    if kind == PackageKind::PortableZip && previous.is_some_and(|record| record.kind == PackageKind::PortableZip) {
        return Ok(None);
    }
    if let Some(inst) = existing {
        let exe = Path::new(&inst.location);
        return Ok(match kind {
            PackageKind::Msi => exe.parent().filter(|_| exe.is_absolute()).map(Path::to_path_buf),
            _ => None,
        });
    }
    let Some(folder) = locations.folder(app.id) else { return Ok(None) };
    if !matches!(kind, PackageKind::Msi | PackageKind::PortableZip) || process::in_sandbox() {
        return Err("custom install folders aren't supported on this system".into());
    }
    let parent = selected_folder(folder, kind != PackageKind::Msi)?;
    Ok(Some(if kind == PackageKind::Msi { parent.join(app.name) } else { parent }))
}

/// The folder a new install goes inside when the person hasn't chosen one, so the UI can show
/// that folder instead of only saying "System default". The answer follows the package: a
/// Windows MSI installs for everyone into `Program Files\Open3D\<App>`, and a 32-bit package
/// into the 32-bit Program Files, so `arch` is the package's architecture when the release names
/// one and `None` (the release is unknown) answers nothing. A portable ZIP unpacks into the
/// launcher's own `apps` folder. A macOS app goes wherever the installer would put it
/// (`/Applications`, or `~/Applications` when the system folder is not writable). Formats the
/// system itself manages (the distro's packages on Linux, Flatpak) have no single folder, so
/// they answer `None`.
pub fn default_folder(kind: PackageKind, arch: Option<Arch>, paths: &Paths) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        match kind {
            PackageKind::Msi => match arch {
                Some(Arch::X86) => {
                    Some(PathBuf::from(std::env::var_os("ProgramFiles(x86)").or_else(|| std::env::var_os("ProgramFiles"))?).join("Open3D"))
                }
                Some(_) => Some(PathBuf::from(std::env::var_os("ProgramFiles")?).join("Open3D")),
                None => None,
            },
            PackageKind::PortableZip => Some(paths.apps()),
            PackageKind::MaxBundle => crate::max_bundle::plugins_dir(),
        }
    }
}

fn selected_folder(folder: &str, writable: bool) -> Result<PathBuf, String> {
    let path = if folder == "~" {
        dirs::home_dir().ok_or("no home folder")?
    } else if let Some(relative) = folder.strip_prefix("~/") {
        dirs::home_dir().ok_or("no home folder")?.join(relative)
    } else {
        PathBuf::from(folder)
    };
    let invalid = |reason: &str| format!("couldn't use install folder {}: {reason}", path.display());
    if folder.chars().any(|c| c.is_control() || (cfg!(windows) && c == '"')) {
        return Err(invalid("the path contains invalid characters"));
    }
    if !path.is_absolute() {
        return Err(invalid("choose an absolute path"));
    }
    if !path.is_dir() {
        return Err(invalid("the folder is unavailable. Create it or reconnect its drive first"));
    }
    if writable {
        write_probe(&path).map_err(|e| invalid(&e.to_string()))?;
    }
    Ok(path)
}

/// Creates and removes a scratch file in `dir`, the way a real write would be refused.
fn write_probe(dir: &Path) -> std::io::Result<()> {
    let probe = dir.join(format!(
        ".open3d-launcher-write-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
    ));
    drop(std::fs::OpenOptions::new().write(true).create_new(true).open(&probe)?);
    std::fs::remove_file(&probe)
}

/// The folder a new macOS app goes inside: `system` (`/Applications`) when this account can
/// create files there, else `Applications` in `home`. It creates nothing, so the install-folder
/// hint can ask on a redraw: `write_probe` takes a fresh name with `create_new`, so a link
/// planted in `/Applications` is never followed and no existing file is touched. The installer
/// creates `~/Applications` itself when it gets that answer.
#[cfg(any(target_os = "macos", test))]
fn mac_applications_dir(system: &Path, home: Option<&Path>) -> Option<PathBuf> {
    if write_probe(system).is_ok() {
        return Some(system.to_path_buf());
    }
    home.map(|home| home.join("Applications"))
}

/// Where `asset` is downloaded to (and its partial file kept while unfinished).
pub(crate) fn download_path(paths: &Paths, asset: &Asset) -> PathBuf {
    paths.downloads().join(&asset.name)
}

/// The package format to install `app` with: the format of the copy that's there (an update
/// stays a `.deb` update), else the user's choice, else the best this system supports.
///
/// Returns an empty `Vec` when `app` is already installed outside the launcher (such as an AUR
/// or repository Arch package) and must not be updated or replaced by the launcher.
pub(crate) fn choose_kinds(app: &SuiteApp, existing: Option<&Installed>, record: Option<&Record>, preferred: &[PackageKind]) -> Vec<PackageKind> {
    choose_kinds_on(app, existing, record, preferred, &SystemKinds::now())
}

/// What this system can install: its package formats ([`available_kinds`]) and whether pacman can
/// take the release's own Arch package. Asking can run a program on the host (inside the Flatpak
/// sandbox), so a question asked for every app on every check keeps one for a while.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SystemKinds {
    pub available: Vec<PackageKind>,
    pub can_pacman: bool,
}

impl SystemKinds {
    /// Asks this system now.
    pub(crate) fn now() -> Self {
        let available = available_kinds();
        Self { available, can_pacman: false }
    }
}

/// [`choose_kinds`] on what `system` can install, without asking it again.
pub(crate) fn choose_kinds_on(
    app: &SuiteApp,
    existing: Option<&Installed>,
    record: Option<&Record>,
    preferred: &[PackageKind],
    system: &SystemKinds,
) -> Vec<PackageKind> {
    choose_kinds_with(app, existing, record, preferred, &system.available, system.can_pacman)
}

pub(crate) fn choose_kinds_with(
    _app: &SuiteApp,
    existing: Option<&Installed>,
    record: Option<&Record>,
    preferred: &[PackageKind],
    available: &[PackageKind],
    _can_pacman: bool,
) -> Vec<PackageKind> {
    if let Some(inst) = existing {
        let kind = match inst.source {
            InstallSource::Launcher => record.map(|r| r.kind),
            InstallSource::Bundle => Some(PackageKind::MaxBundle),
        };
        if let Some(k) = kind.filter(|k| available.contains(k)) {
            return vec![k];
        }
    }
    let mut kinds: Vec<PackageKind> = preferred.iter().copied().filter(|k| available.contains(k)).collect();
    for k in available {
        if !kinds.contains(k) {
            kinds.push(*k);
        }
    }
    kinds
}

/// Package formats this machine can install, best first.
pub(crate) fn available_kinds() -> Vec<PackageKind> {
    #[cfg(windows)]
    return vec![PackageKind::MaxBundle];
    #[allow(unreachable_code)]
    Vec::new()
}

/// Fetches `asset` into the download folder, checked against its published SHA-256: the feed's
/// or GitHub's per-file digest, else the release's `SHA256SUMS.txt`. A release with neither is
/// refused before anything is fetched: a file nothing vouches for is never installed (#90).
/// Returns the file and the checksum it was checked against.
pub(crate) fn download(release: &Release, asset: &Asset, job: &mut Job<'_>) -> Result<(PathBuf, String), String> {
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
    Ok((dest, sha))
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

/// Removes an installed copy. `phase` hears when the removal has to wait for another install;
/// `stop` cancels it while it waits.
pub(crate) fn uninstall(
    app: &SuiteApp,
    installed: &Installed,
    record: Option<&Record>,
    paths: &Paths,
    stop: &Stop,
    phase: &mut dyn FnMut(JobPhase),
) -> Result<(), String> {
    let _ = (record, stop, &phase, paths);
    let path = PathBuf::from(&installed.location);
    if matches!(installed.source, InstallSource::Launcher) && process::is_running(&path) {
        return Err(format!("{} is running. Quit it, then try again.", app.name));
    }
    match installed.source {
        #[cfg(windows)]
        InstallSource::Launcher if record.is_some_and(|r| r.kind == PackageKind::PortableZip) => {
            crate::portable::remove(app, record.ok_or("the launcher has no record of these files")?)
        }
        InstallSource::Bundle => crate::max_bundle::uninstall(app, &path),
        InstallSource::Launcher if record.is_some_and(|r| r.kind == PackageKind::MaxBundle) => crate::max_bundle::uninstall(app, &path),
        #[cfg(windows)]
        // A tarball the launcher unpacked into its own folder: take the tree out and leave the
        // app's settings (`home/`) alone, so a reinstall finds its projects where they were.
        other => Err(format!("{} can't be removed from here", other.label())),
    }
}

/// Starts an installed app with `args` (the user's launch flags) on its command line.
pub(crate) fn launch(app: &SuiteApp, installed: &Installed, paths: &Paths, args: &[String]) -> Result<(), String> {
    let _ = paths;
    let path = Path::new(&installed.location);
    // Every app is told where the shared version-choice file is (see `craft_update::pins`), so
    // it finds it even when its own data folder points elsewhere: a redirected home, a sandbox,
    // or a launcher whose data folder moved.
    let pins = craft_update::pins::Pins::default_path();
    let env: Vec<(&str, &std::ffi::OsStr)> = pins.iter().map(|p| (craft_update::pins::PATH_ENV, p.as_os_str())).collect();
    if installed.source == InstallSource::Bundle || path.join("PackageContents.xml").is_file() {
        return crate::max_bundle::launch(app).map_err(|e| format!("Couldn't open {}: {e}", app.name));
    }
    match installed.source {
        // `open` starts the app through Launch Services, which gives it the user's real home, so
        // it finds the file at its default path; not every macOS `open` takes an environment.
        // A tarball the launcher unpacked into its own folder: run it here, against its own
        // home, rather than on the host through `flatpak-spawn` (which is what `launch_binary`
        // would do inside the sandbox). Its settings then land beside it instead of in the
        // launcher's or the host's home folder.
        _ if cfg!(target_os = "macos") && path.extension().is_some_and(|e| e == "app") => process::open_app(path, args),
        _ => {
            if !path.exists() {
                return Err(format!("{} isn't where it was installed ({})", app.name, path.display()));
            }
            process::launch_binary_with_env(path, args, &env)
        }
    }
    .map_err(|e| format!("Couldn't open {}: {e}", app.name))
}

#[cfg(windows)]
pub(crate) mod windows {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 32-bit package installs under the 32-bit Program Files; an unknown architecture answers
    /// nothing, so the UI says "System default" instead of guessing the destination.
    #[cfg(windows)]
    #[test]
    fn a_32_bit_package_uses_the_32_bit_program_files_and_an_unknown_architecture_answers_nothing() {
        let paths = Paths::under(std::path::Path::new(r"C:\scratch"));
        let x86 = std::env::var_os("ProgramFiles(x86)").or_else(|| std::env::var_os("ProgramFiles")).map(PathBuf::from).unwrap();
        assert_eq!(default_folder(PackageKind::Msi, Some(Arch::X86), &paths), Some(x86.join("Open3D")));
        assert_eq!(default_folder(PackageKind::Msi, None, &paths), None);
    }

    /// The macOS default-folder answer is asked on a redraw, so it must leave the disk as it was:
    /// nothing left in a writable `/Applications`, no `~/Applications` created when it is not.
    #[test]
    fn the_mac_default_folder_lookup_creates_nothing() {
        let system = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        assert_eq!(mac_applications_dir(system.path(), Some(home.path())), Some(system.path().to_path_buf()));
        assert_eq!(std::fs::read_dir(system.path()).unwrap().count(), 0, "the write probe was left behind");

        let unwritable = system.path().join("missing");
        assert_eq!(mac_applications_dir(&unwritable, Some(home.path())), Some(home.path().join("Applications")));
        assert!(!home.path().join("Applications").exists(), "a folder hint must not create ~/Applications");
        assert_eq!(mac_applications_dir(&unwritable, None), None);
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
