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
pub struct Job<'a> {
    pub http: &'a Http,
    pub paths: &'a Paths,
    pub stop: &'a Stop,
    pub phase: &'a mut dyn FnMut(JobPhase),
    pub install_locations: &'a InstallLocations,
    /// Write over whatever [`occupied`] found.
    pub replace: bool,
    /// The exact path [`occupied`] reported when the user agreed to replace it.
    pub approved_path: Option<&'a Path>,
    /// Set only when the person chose this older version by name over a newer installed copy.
    pub downgrade: Option<Downgrade<'a>>,
    /// The batch of installs this one belongs to (an "update all"): where the platform needs a
    /// permission prompt, the whole batch shares one. `None` for a job on its own.
    pub batch: Option<crate::queue::Gather>,
}

/// A job's place in its batch (see [`Job::batch`]): the installer queue takes it where the batch
/// shares one call ([`crate::queue::Queue::batched_when`]); a job that ends before that — failed,
/// cancelled, or needing no prompt at all — gives the place back on its way out, so the rest of
/// the batch never waits for it. [`install_at`](fn@install_at) takes it for the installer routes;
/// the backend holds it the same way for every earlier way out of a job.
pub(crate) struct BatchPlace(pub(crate) Option<crate::queue::Gather>);

impl BatchPlace {
    pub(crate) fn take(&mut self) -> Option<crate::queue::Gather> {
        self.0.take()
    }
}

impl Drop for BatchPlace {
    fn drop(&mut self) {
        if let Some(gather) = self.0.take() {
            gather.leave();
        }
    }
}

/// How a downgrade the person chose is undone if it fails.
#[derive(Clone, Copy)]
pub enum Downgrade<'a> {
    /// The installed version's own release and package, downloaded and checked before a downgrade
    /// changes anything, so a failed Windows Installer downgrade can put it back.
    Reinstall { previous: &'a Release, asset: &'a Asset },
    /// The installed copy stays until the older one is in place (see [`keeps_old_copy`]). No
    /// older package is needed.
    KeepOldCopy,
}

/// Whether going back over `existing` keeps the old copy until the new one is in place, the same
/// way an update does. A macOS bundle is moved aside and moved back if the swap fails. An
/// AppImage the launcher installed (`previous` records it) is renamed in whole, and the old file
/// is removed only after that. A tarball is left out: its `bin` and `share` swap one at a time.
pub fn keeps_old_copy(kind: PackageKind, existing: Option<&Installed>, previous: Option<&Record>) -> bool {
    let source = existing.map(|inst| inst.source);
    match kind {
        PackageKind::Dmg => source == Some(InstallSource::MacApp),
        PackageKind::AppImage => source == Some(InstallSource::Launcher) && previous.is_some_and(|r| r.kind == PackageKind::AppImage),
        _ => false,
    }
}

/// Whether `downgrade` has a way back for this kind of install.
fn downgrade_is_safe(downgrade: Option<Downgrade<'_>>, kind: PackageKind, existing: Option<&Installed>, previous: Option<&Record>) -> bool {
    match downgrade {
        None => true,
        Some(Downgrade::Reinstall { .. }) => kind == PackageKind::Msi && existing.is_some_and(|inst| inst.source == InstallSource::Msi),
        Some(Downgrade::KeepOldCopy) => keeps_old_copy(kind, existing, previous),
    }
}

/// Downloads and installs `asset` of `release`. Returns a record when the launcher owns the
/// result (portable ZIP, AppImage, macOS bundle, tarball); MSI installs are found in the registry.
/// `previous` is the launcher's record of the copy being replaced, if it installed it.
pub fn install(
    app: &SuiteApp,
    release: &Release,
    asset: &Asset,
    kind: PackageKind,
    existing: Option<&Installed>,
    previous: Option<&Record>,
    job: &mut Job<'_>,
) -> Result<Option<Record>, String> {
    install_at(app, release, asset, kind, existing, previous, job, None)
}

type RecordCommit<'a> = Option<&'a mut dyn FnMut(&Record) -> Result<(), String>>;

#[allow(clippy::too_many_arguments)]
pub fn install_at(
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
    let mut commit = commit;
    // Every early way out below gives the job's place in its batch back (see [`BatchPlace`]).
    let mut batch = BatchPlace(job.batch.take());
    let destination = destination(app, kind, existing, previous, job.install_locations)?;
    #[cfg(windows)]
    let destination = msi_update_folder(app, kind, existing, destination);
    if let Some(inst) = existing
        && inst.source.launcher_can_update()
        && process::is_running(Path::new(&inst.location))
    {
        return Err(format!("{} is running. Quit it, then try again.", app.name));
    }
    if kind == PackageKind::MaxBundle && crate::max_bundle::max_running() {
        return Err(crate::max_bundle::max_running_error());
    }
    if kind == PackageKind::Flatpak
        && let Some(installed) = existing.filter(|installed| installed.source == InstallSource::Flatpak && installed.externally_managed)
    {
        return Err(managed_elsewhere(app, installed, false).unwrap_or_else(|| "Update this app through its Flatpak repository".into()));
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
    if !downgrade_is_safe(job.downgrade, kind, existing, previous) {
        return Err(format!("Going back to an older version of {} isn't available for this kind of install yet.", app.name));
    }
    log::info!("{}: starting {} to {} ({})", app.name, if existing.is_some() { "update" } else { "install" }, release.version_label(), kind.label());
    #[cfg(windows)]
    let msi_scope = if kind == PackageKind::Msi {
        let updated = crate::msi_context::update_scope(
            existing.is_some_and(|inst| inst.source == InstallSource::Msi),
            existing.and_then(|inst| inst.package.as_deref()),
            crate::msi_context::probe,
            |code| crate::detect::windows::record_scope(app, code),
        )?;
        // A fresh install is machine-wide (Program Files); an update keeps the context of the copy it replaces.
        crate::msi_context::install_scope(updated, existing.is_none())
    } else {
        None
    };
    // The package to put back comes first: without it, nothing is removed.
    let rollback = match job.downgrade {
        Some(Downgrade::Reinstall { previous, asset }) => {
            let (file, sha256) = download(previous, asset, job)?;
            Some((file, sha256, previous.version_label()))
        }
        Some(Downgrade::KeepOldCopy) | None => None,
    };
    let (file, sha256) = download(release, asset, job)?;
    log::info!("{}: download ready ({}), running installer...", app.name, file.display());
    // Past this point the install can't be interrupted halfway.
    (job.phase)(JobPhase::Installing);
    let version = release.version_label();
    let result = match kind {
        // No team to check the signature against: refuse rather than install it unchecked.
        #[cfg(windows)]
        PackageKind::Msi => {
            // An update lands in the same Windows Installer context as the copy being replaced:
            // per-machine needs elevation, per-user must not duplicate itself per-machine. A
            // fresh install passes no context and takes the package's own default.
            match &rollback {
                Some((previous, previous_sha256, previous_version)) => windows::downgrade_msi(
                    app,
                    windows::Package { file: &file, sha256: &sha256, version: &version },
                    windows::Package { file: previous, sha256: previous_sha256, version: previous_version },
                    destination.as_deref(),
                    msi_scope,
                    job.stop,
                    &mut *job.phase,
                ),
                None => windows::run_msi(
                    app,
                    windows::Package { file: &file, sha256: &sha256, version: &version },
                    destination.as_deref(),
                    msi_scope,
                    batch.take(),
                    job.stop,
                    &mut *job.phase,
                ),
            }
            .map(|_| None)
        }
        #[cfg(windows)]
        PackageKind::PortableZip => {
            crate::portable::install(app, &file, &version, previous, job.paths, destination.as_deref(), job.replace, &mut |record| {
                if release.version.is_some() && crate::detect::windows::executable_version(&record.path).as_ref() != release.version.as_ref() {
                    return Err("The portable executable does not match the selected version. The installation was rolled back.".into());
                }
                match commit.as_deref_mut() {
                    Some(commit) => commit(record),
                    None => Ok(()),
                }
            })
            .map(Some)
        }
        PackageKind::MaxBundle => crate::max_bundle::install(app, &file, &version).map(Some),
        other => Err(format!("{} isn't supported on this system", other.label())),
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
        if let Some((previous, ..)) = &rollback {
            let _ = std::fs::remove_file(previous);
        }
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
pub fn occupied(
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
        PackageKind::Dmg => {
            folder.and_then(|f| app.mac_bundle_names().map(|n| f.join(format!("{n}.app"))).find(|t| std::fs::symlink_metadata(t).is_ok()))
        }
        _ => None,
    };
    Ok(target.filter(|t| std::fs::symlink_metadata(t).is_ok()))
}

/// An MSI update keeps the folder of the copy it replaces only when that copy was found in the
/// record's own scope. A path found by the launch-only fallback, in another scope, is never passed
/// to the installer: the update then keeps the record's context and the package's own folder.
#[cfg(windows)]
fn msi_update_folder(app: &SuiteApp, kind: PackageKind, existing: Option<&Installed>, destination: Option<PathBuf>) -> Option<PathBuf> {
    match existing {
        Some(inst) if kind == PackageKind::Msi && inst.source == InstallSource::Msi => {
            let in_scope = inst.package.as_deref().is_some_and(|code| crate::detect::windows::location_is_in_record_scope(app, code, &inst.location));
            keep_if_in_scope(destination, in_scope)
        }
        _ => destination,
    }
}

#[cfg(windows)]
fn keep_if_in_scope(destination: Option<PathBuf>, in_scope: bool) -> Option<PathBuf> {
    destination.filter(|_| in_scope)
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
            PackageKind::AppImage => previous.filter(|r| r.kind == kind).and_then(|r| r.path.parent()).map(Path::to_path_buf),
            PackageKind::Msi => exe.parent().filter(|_| exe.is_absolute()).map(Path::to_path_buf),
            _ => None,
        });
    }
    let Some(folder) = locations.folder(app.id) else { return Ok(None) };
    if !matches!(kind, PackageKind::Dmg | PackageKind::AppImage | PackageKind::Msi | PackageKind::PortableZip) || process::in_sandbox() {
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
            _ => None,
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

#[cfg(any(windows, test))]
fn msi_folder_argument(path: &Path) -> Result<String, String> {
    let value = path.to_str().ok_or("unusual characters in a path")?;
    if value.chars().any(|c| c == '"' || c.is_control()) {
        return Err(format!("couldn't use install folder {value}: the path contains invalid characters"));
    }
    Ok(format!("INSTALLFOLDER=\"{value}\""))
}

/// The Windows Installer context an update must keep. A per-machine install elevates and writes
/// HKLM registrations; a per-user one must not silently become a second, per-machine copy (or
/// the other way round). A fresh install is always `PerMachine`.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MsiScope {
    PerUser,
    PerMachine,
}

/// The `msiexec` properties that ask a dual-scope package for `scope`. Passing the context
/// explicitly on every update also keeps a per-machine update working when the package's own
/// default has since changed.
///
/// `ALLUSERS=1` is what the current per-machine-only packages already set themselves, so it
/// changes nothing for them; `ALLUSERS=2` with `MSIINSTALLPERUSER=1` is the documented
/// single-package way to request a per-user install of a dual-scope package. A per-user
/// registered copy can only come from a dual-scope package, so its updates can safely ask for
/// one.
#[cfg(any(windows, test))]
fn msi_scope_arguments(scope: Option<MsiScope>) -> Vec<&'static str> {
    match scope {
        Some(MsiScope::PerUser) => vec!["ALLUSERS=2", "MSIINSTALLPERUSER=1"],
        Some(MsiScope::PerMachine) => vec!["ALLUSERS=1"],
        None => Vec::new(),
    }
}

/// Where `asset` is downloaded to (and its partial file kept while unfinished).
pub fn download_path(paths: &Paths, asset: &Asset) -> PathBuf {
    paths.downloads().join(&asset.name)
}

/// The package format to install `app` with: the format of the copy that's there (an update
/// stays a `.deb` update), else the user's choice, else the best this system supports.
///
/// Returns an empty `Vec` when `app` is already installed outside the launcher (such as an AUR
/// or repository Arch package) and must not be updated or replaced by the launcher.
pub fn choose_kinds(app: &SuiteApp, existing: Option<&Installed>, record: Option<&Record>, preferred: &[PackageKind]) -> Vec<PackageKind> {
    choose_kinds_on(app, existing, record, preferred, &SystemKinds::now())
}

/// What this system can install: its package formats ([`available_kinds`]) and whether pacman can
/// take the release's own Arch package. Asking can run a program on the host (inside the Flatpak
/// sandbox), so a question asked for every app on every check keeps one for a while.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemKinds {
    pub available: Vec<PackageKind>,
    pub can_pacman: bool,
}

impl SystemKinds {
    /// Asks this system now.
    pub fn now() -> Self {
        let available = available_kinds();
        let can_pacman = available.contains(&PackageKind::ArchPkg) || can_pacman();
        Self { available, can_pacman }
    }
}

/// [`choose_kinds`] on what `system` can install, without asking it again.
pub fn choose_kinds_on(
    app: &SuiteApp,
    existing: Option<&Installed>,
    record: Option<&Record>,
    preferred: &[PackageKind],
    system: &SystemKinds,
) -> Vec<PackageKind> {
    choose_kinds_with(app, existing, record, preferred, &system.available, system.can_pacman)
}

pub(crate) fn choose_kinds_with(
    app: &SuiteApp,
    existing: Option<&Installed>,
    record: Option<&Record>,
    preferred: &[PackageKind],
    available: &[PackageKind],
    can_pacman: bool,
) -> Vec<PackageKind> {
    // Formats the launcher can't verify for this app (a macOS build with no `mac_team`) never count.
    let available: Vec<PackageKind> = available.iter().copied().filter(|k| app.installs(*k)).collect();
    let available = available.as_slice();
    if let Some(inst) = existing {
        let kind = match inst.source {
            InstallSource::Launcher => record.map(|r| r.kind),
            InstallSource::MacApp => Some(PackageKind::Dmg),
            InstallSource::Msi => Some(PackageKind::Msi),
            InstallSource::Deb => Some(PackageKind::Deb),
            InstallSource::Rpm => Some(PackageKind::Rpm),
            // The release's own Arch package is updated like a .deb. AUR builds and repository
            // packages are updated outside the launcher (see `managed_elsewhere`); the launcher
            // opens and removes them but never replaces them.
            InstallSource::Pacman if is_release_arch_package(app, inst) => Some(PackageKind::ArchPkg),
            InstallSource::Pacman => return Vec::new(),
            // Repository-managed Flatpaks are externally managed: the launcher will not
            // replace them with a downloaded local .flatpak bundle.
            InstallSource::Flatpak if inst.externally_managed => return Vec::new(),
            InstallSource::Flatpak => Some(PackageKind::Flatpak),
            InstallSource::AppImage => Some(PackageKind::AppImage),
            InstallSource::Binary => Some(PackageKind::TarGz),
            InstallSource::Bundle => Some(PackageKind::MaxBundle),
        };
        if let Some(k) = kind.filter(|k| available.contains(k) || (*k == PackageKind::ArchPkg && can_pacman)) {
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

/// Whether this system has a working pacman installation and pacman database.
fn can_pacman() -> bool {
    #[cfg(not(target_os = "linux"))]
    return false;
}

/// Whether a pacman install is the release's own Arch package rather than an AUR build. The
/// release names it by the bundle id (`io.github.carlesllavata.<app>`), which no AUR package uses
/// (they are `<app>`, `<app>-bin` and `<app>-git`), so an AUR helper never mistakes it
/// for one of its own and rebuilds it from source.
pub fn is_release_arch_package(app: &SuiteApp, installed: &Installed) -> bool {
    installed.source == InstallSource::Pacman && installed.package.as_deref() == Some(&app.bundle_id())
}

/// Package formats this machine can install, best first.
pub fn available_kinds() -> Vec<PackageKind> {
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

/// When `installed` is owned by something the launcher doesn't drive, what to tell the user
/// instead of installing a second copy. Arch packages other than the release's own are removed
/// through pacman like any system package, but updated by whatever they came from: a system
/// update for a repository package (Omarchy's, Chaotic-AUR, ...), the user's AUR helper (which
/// builds as the user and can't run behind the password prompt) for anything else.
pub fn managed_elsewhere(app: &SuiteApp, installed: &Installed, uninstall: bool) -> Option<String> {
    managed_elsewhere_with(app, installed, uninstall, pacman_repo)
}

/// Whether the launcher leaves `installed` to whatever installed it, as [`managed_elsewhere`]
/// says, without asking pacman which repository to name in the message: for a question that only
/// needs the answer, such as whether an update can be installed.
pub fn is_managed_elsewhere(app: &SuiteApp, installed: &Installed) -> bool {
    managed_elsewhere_with(app, installed, false, |_| None).is_some()
}

fn managed_elsewhere_with(app: &SuiteApp, installed: &Installed, uninstall: bool, repo_fn: impl Fn(&str) -> Option<String>) -> Option<String> {
    match installed.source {
        InstallSource::Pacman if !uninstall && !is_release_arch_package(app, installed) => {
            let pkg = installed.package.clone().unwrap_or_else(|| app.id.to_string());
            Some(pacman_update_hint(app, &pkg, repo_fn(&pkg).as_deref()))
        }
        InstallSource::Flatpak if !uninstall && installed.externally_managed => {
            let default_id = app.bundle_id();
            let pkg = installed.package.as_deref().unwrap_or(&default_id);
            let scope = crate::packages::flatpak_scope(installed.installation.as_deref());
            Some(format!("{} was installed from a Flatpak repository. Update it with flatpak update {scope} {pkg}", app.name))
        }
        InstallSource::Binary => Some(format!("{} was installed outside the launcher. Update or remove it the way it was installed.", app.name)),
        _ => None,
    }
}

/// How to update an Arch package the launcher doesn't own: `repo` is the sync repository it
/// comes from, or `None` for a foreign (AUR) package.
fn pacman_update_hint(app: &SuiteApp, pkg: &str, repo: Option<&str>) -> String {
    let helper = ["paru", "yay"].into_iter().find(|h| which(h).is_some());
    let cmd = match helper {
        Some(h) => format!("{h} -S {pkg}"),
        None => format!("your AUR helper (for example: yay -S {pkg})"),
    };
    if let Some(repo) = repo {
        return format!("{} was installed from the {repo} repository or the AUR ({pkg}). Update it with {cmd} or sudo pacman -Syu", app.name);
    }
    format!("{} was installed from the AUR ({pkg}). Update it with {cmd}", app.name)
}

/// The sync repository (`extra`, `omarchy`, ...) that carries the installed package `pkg`, or
/// `None` when no configured repository does (an AUR or locally built package) or pacman can't
/// be asked.
fn pacman_repo(pkg: &str) -> Option<String> {
    let pacman = which("pacman")?;
    let t = std::time::Duration::from_secs(5);
    // For packages carried by a configured sync repository, ask pacman -Si which repo carries it.
    // pacman translates its field names; the C locale keeps "Repository" parseable.
    let mut si = process::command(&pacman);
    si.args(["-Si", "--", pkg]).env("LC_ALL", "C");
    let out = process::run_command(si, "pacman", t).ok()?;
    parse_pacman_repo(&out)
}

/// The `Repository` field of `pacman -Si` output (the first one, when several repos carry it:
/// that is the one pacman installs from).
fn parse_pacman_repo(si: &str) -> Option<String> {
    si.lines().find_map(|l| {
        let (key, value) = l.split_once(':')?;
        (key.trim() == "Repository").then(|| value.trim().to_string()).filter(|r| !r.is_empty())
    })
}

/// Removes an installed copy. `phase` hears when the removal has to wait for another install;
/// `stop` cancels it while it waits.
pub fn uninstall(
    app: &SuiteApp,
    installed: &Installed,
    record: Option<&Record>,
    paths: &Paths,
    stop: &Stop,
    phase: &mut dyn FnMut(JobPhase),
) -> Result<(), String> {
    let _ = (record, stop, &phase, paths);
    let path = PathBuf::from(&installed.location);
    if let Some(msg) = managed_elsewhere(app, installed, true) {
        return Err(msg);
    }
    if matches!(installed.source, InstallSource::MacApp | InstallSource::Msi | InstallSource::AppImage | InstallSource::Launcher)
        && process::is_running(&path)
    {
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
        InstallSource::Msi => windows::uninstall_msi(app, stop, phase),
        // A tarball the launcher unpacked into its own folder: take the tree out and leave the
        // app's settings (`home/`) alone, so a reinstall finds its projects where they were.
        other => Err(format!("{} can't be removed from here", other.label())),
    }
}

/// Starts an installed app with `args` (the user's launch flags) on its command line.
pub fn launch(app: &SuiteApp, installed: &Installed, paths: &Paths, args: &[String]) -> Result<(), String> {
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
        InstallSource::Flatpak => {
            let mut cmd = process::flatpak_command();
            cmd.arg("run");
            cmd.args(process::env_args(&env));
            // The id detection found it under, which may be one of the app's older ones.
            cmd.arg(installed.package.clone().unwrap_or_else(|| app.bundle_id()));
            // After the app's id: flags before it would go to `flatpak run` itself.
            cmd.args(args);
            process::spawn_detached(cmd)
        }
        // `open` starts the app through Launch Services, which gives it the user's real home, so
        // it finds the file at its default path; not every macOS `open` takes an environment.
        InstallSource::MacApp => process::open_app(path, args),
        // A tarball the launcher unpacked into its own folder: run it here, against its own
        // home, rather than on the host through `flatpak-spawn` (which is what `launch_binary`
        // would do inside the sandbox). Its settings then land beside it instead of in the
        // launcher's or the host's home folder.
        _ if cfg!(target_os = "macos") && path.extension().is_some_and(|e| e == "app") => process::open_app(path, args),
        _ => {
            if !path.exists() {
                if cfg!(windows) && installed.source == InstallSource::Msi && path.components().count() < 2 {
                    return Err(format!(
                        "{} is registered with Windows, but the launcher can't find where it put the program. Reinstall it from here.",
                        app.name
                    ));
                }
                return Err(format!("{} isn't where it was installed ({})", app.name, path.display()));
            }
            process::launch_binary_with_env(path, args, &env)
        }
    }
    .map_err(|e| format!("Couldn't open {}: {e}", app.name))
}

/// Finds a program on `PATH`.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

/// How the error of an installer call shared by several apps ends (see `linux::shared_outcome`).
const SHARED_CALL: &str = "apps together. Try them one at a time.";

/// Whether an install failed in one installer call shared with other apps (Linux packages
/// installed together). Such a call can fail after it has installed some of its apps; any other
/// failure is the app's own.
pub fn failed_in_shared_call(error: &str) -> bool {
    cfg!(target_os = "linux") && error.ends_with(SHARED_CALL)
}

/// The system's installer runs one job at a time; the others wait their turn here. The backend
/// starts each batch of installs (an "update all") with [`Queue::gather`] on it.
#[cfg(any(target_os = "linux", windows))]
pub(crate) static INSTALLER: crate::queue::Queue = crate::queue::Queue::new();

/// For [`INSTALLER`]: reports [`JobPhase::Waiting`] while a job waits, then `working` again.
#[cfg(any(target_os = "linux", windows))]
fn turn<'a>(phase: &'a mut dyn FnMut(JobPhase), working: JobPhase) -> impl FnMut(bool) + 'a {
    move |waiting| phase(if waiting { JobPhase::Waiting } else { working })
}

/// For [`INSTALLER`]: whether the user cancelled the job while it waits.
#[cfg(any(target_os = "linux", windows))]
fn cancelled(stop: &Stop) -> impl Fn() -> bool + '_ {
    || stop.check().is_err_and(|e| e == open3d_launcher_core::protocol::CANCELLED)
}

/// What a Windows Installer exit code means for the job that ran it.
#[cfg(any(windows, test))]
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

/// Going back to an older version of a Windows Installer app.
#[cfg(any(windows, test))]
mod msi_downgrade;

/// Helpers for Windows Installer PowerShell scripts running under elevation.
#[cfg(any(windows, test))]
mod msi_batch;
/// One permission prompt for a batch of per-machine packages: the launcher's own exe, elevated
/// once, running Windows Installer on each checked package.
#[cfg(any(windows, test))]
pub mod msi_session;
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

/// Where `msi_command` asks Windows Installer to write its log.
fn msi_log(msi: &Path) -> PathBuf {
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
    use open3d_launcher_core::protocol::CANCELLED;

    use super::{
        INSTALLER, JobPhase, MsiScope, Path, PathBuf, Stop, SuiteApp, cancelled, explain, msi_batch, msi_command, msi_downgrade, msi_folder_argument,
        msi_log, msi_scope_arguments, msi_session, process, turn,
    };

    pub(super) fn msiexec() -> PathBuf {
        std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from).join(r"System32\msiexec.exe")
    }

    /// Runs an MSI with a progress window and no restart. Windows asks for permission itself.
    /// `scope`, from the copy being replaced's registration, keeps the update in the same
    /// Windows Installer context (see [`MsiScope`]). A per-machine package with a `batch` joins
    /// it instead: the whole batch shares one permission prompt ([`run_batched`]). Only
    /// [`install_at`](super::install_at) calls it, so it stays as private as its [`Package`].
    pub(super) fn run_msi(
        app: &SuiteApp,
        package: Package<'_>,
        destination: Option<&Path>,
        scope: Option<MsiScope>,
        batch: Option<crate::queue::Gather>,
        stop: &Stop,
        phase: &mut dyn FnMut(JobPhase),
    ) -> Result<(), String> {
        if let Some(gather) = batch {
            if scope == Some(MsiScope::PerMachine) {
                return run_batched(Batched { app, package, destination, scope }, gather, stop, phase);
            }
            // A per-user install asks Windows for nothing: its place in the batch goes back at
            // once, so the per-machine members don't wait for it.
            gather.leave();
        }
        in_turn(stop, phase, JobPhase::Installing, || run_direct(package.file, destination, scope))
    }

    /// One app's install, as the batch session runs it.
    struct Batched<'a> {
        app: &'a SuiteApp,
        package: Package<'a>,
        destination: Option<&'a Path>,
        scope: Option<MsiScope>,
    }

    /// One permission prompt for a batch of per-machine packages: the members meet in one call
    /// of the installer queue, the one whose call it is starts the elevated session
    /// ([`msi_session`]), and every member hears its own installer's exit code. A declined
    /// prompt cancels the whole batch; a session that cannot start sends the members back to
    /// installing one by one, the way a single app always installs.
    fn run_batched(job: Batched<'_>, gather: crate::queue::Gather, stop: &Stop, phase: &mut dyn FnMut(JobPhase)) -> Result<(), String> {
        let Batched { app, package, destination, scope } = job;
        let target = match msi_session::entry(app, &package, destination, scope) {
            Ok(target) => target,
            Err(e) => {
                log::warn!("{}: {e}; installing it by itself", app.name);
                gather.leave();
                return in_turn(stop, phase, JobPhase::Installing, || run_direct(package.file, destination, scope));
            }
        };
        let outcome = INSTALLER.batched_when(
            Some(gather),
            msi_session::command(),
            target,
            &cancelled(stop),
            &mut turn(phase, JobPhase::Installing),
            &msi_session::call,
        );
        if outcome.as_ref().err().is_some_and(|e| e == msi_session::NO_SESSION) {
            log::warn!("{}: the batch session could not start; installing it by itself", app.name);
            return in_turn(stop, phase, JobPhase::Installing, || run_direct(package.file, destination, scope));
        }
        outcome
    }

    /// Runs one MSI by itself: `msiexec` asks for permission itself, and the prompt names the package's
    /// publisher.
    fn run_direct(msi: &Path, destination: Option<&Path>, scope: Option<MsiScope>) -> Result<(), String> {
        use std::os::windows::process::CommandExt;
        let mut command = msi_command(&msiexec(), msi)?;
        if let Some(dir) = destination {
            command.raw_arg(msi_folder_argument(dir)?);
        }
        for argument in msi_scope_arguments(scope) {
            command.raw_arg(argument);
        }
        let status = command.status().map_err(|e| format!("couldn't start the Windows Installer ({e})"))?;
        explain(status.code()).map_err(|e| match status.code() {
            Some(1602) | None => e,
            _ => format!("{e}. Its log is at {}", msi_log(msi).display()),
        })
    }

    /// Runs the launcher's own new MSI for a self-update, with the same progress window and no
    /// restart as [`run_msi`], and waits for it.
    pub fn reinstall(msi: &Path) -> Result<(), String> {
        let status = msi_command(&msiexec(), msi)?.status().map_err(|e| format!("couldn't start the Windows Installer ({e})"))?;
        explain(status.code())
    }

    /// Runs `work` once no other Windows Installer job of ours is running (a second one would
    /// stop with error 1618). Cancelled while it waits, it never runs.
    fn in_turn(stop: &Stop, phase: &mut dyn FnMut(JobPhase), working: JobPhase, work: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        INSTALLER.alone(&cancelled(stop), &mut turn(phase, working), work).unwrap_or_else(|| Err(CANCELLED.into()))
    }

    /// Goes back to `chosen` (its package and version) from a newer installed copy, which the app
    /// packages won't install over: removes it, installs `chosen` in the same folder and context,
    /// and puts `previous` back from its package if that does not finish (see [`msi_downgrade`]).
    /// A downloaded package checked against its release checksum, and its version.
    pub(super) struct Package<'a> {
        pub file: &'a Path,
        pub sha256: &'a str,
        pub version: &'a str,
    }

    /// Goes back to `chosen` from a newer installed copy, which the app packages won't install
    /// over: removes it, installs `chosen` in the same folder and context, and puts `previous`
    /// back if that does not finish (see [`msi_downgrade`]). An all-users copy does all three
    /// under one permission prompt, or not at all; a per-user copy needs none, and runs them one
    /// by one in a single turn of the installer queue.
    pub(super) fn downgrade_msi(
        app: &SuiteApp,
        chosen: Package<'_>,
        previous: Package<'_>,
        destination: Option<&Path>,
        scope: Option<MsiScope>,
        stop: &Stop,
        phase: &mut dyn FnMut(JobPhase),
    ) -> Result<(), String> {
        if scope == Some(MsiScope::PerMachine) {
            let mut elevated = None;
            in_turn(stop, phase, JobPhase::Installing, || {
                elevated = downgrade_elevated(app, &chosen, &previous, destination, scope)?;
                Ok(())
            })?;
            // Separate prompts are no fallback: refusing a later one would leave no app at all.
            return match elevated {
                Some(ended) => msi_downgrade::report(ended, app.name, chosen.version, previous.version),
                None => Err(msi_downgrade::no_session(app.name, chosen.version)),
            };
        }
        // One turn for all three steps: once the copy is removed, nothing waiting in the queue
        // (another install, or a cancel while waiting) can stop the put-back.
        in_turn(stop, phase, JobPhase::Installing, || {
            msi_downgrade::run(msi_downgrade::Steps {
                app: app.name,
                chosen: chosen.version,
                previous: previous.version,
                remove: &mut || remove_registrations(app),
                install_chosen: &mut || run_direct(chosen.file, destination, scope),
                reinstall_previous: &mut || run_direct(previous.file, destination, scope),
            })
        })
    }

    /// The one-prompt route: both packages held unchanged and checked again, then one elevated
    /// session removes, installs and if needed puts back. `None` when that session can't start.
    fn downgrade_elevated(
        app: &SuiteApp,
        chosen: &Package<'_>,
        previous: &Package<'_>,
        destination: Option<&Path>,
        scope: Option<MsiScope>,
    ) -> Result<Option<msi_downgrade::Ended>, String> {
        let _held = (msi_batch::hold(chosen.file, chosen.sha256)?, msi_batch::hold(previous.file, previous.sha256)?);
        let installs = crate::detect::windows::msi_installs(app);
        if installs.is_empty() {
            return Err(format!("{} isn't registered with the Windows Installer", app.name));
        }
        let removals = msi_downgrade::elevated_removals(app.name, installs.iter().map(|m| (m.product_code.as_str(), m.scope)))?;
        let script = msi_downgrade::script(
            msi_batch::text(&msiexec())?,
            &removals,
            &msi_batch::line(chosen.file, destination, scope)?,
            &msi_batch::line(previous.file, destination, scope)?,
        );
        let root =
            std::env::var_os("SystemRoot").map(PathBuf::from).filter(|root| root.is_absolute()).ok_or("SystemRoot is not set to an absolute path")?;
        let powershell = root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let mut command = process::command(&powershell);
        command
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(msi_batch::outer_script(msi_batch::text(&powershell)?, &msi_batch::encoded(&script)));
        log::info!("{}: going back to {} under one permission prompt", app.name, chosen.version);
        let ended = match command.status() {
            Ok(status) => msi_downgrade::ended(status.code()),
            Err(e) => {
                log::warn!("couldn't start PowerShell ({e})");
                msi_downgrade::Ended::LaunchFailed
            }
        };
        Ok((ended != msi_downgrade::Ended::LaunchFailed).then_some(ended))
    }

    pub fn uninstall_msi(app: &SuiteApp, stop: &Stop, phase: &mut dyn FnMut(JobPhase)) -> Result<(), String> {
        in_turn(stop, phase, JobPhase::Removing, || remove_registrations(app).map_err(|removal| removal.error))
    }

    /// Removes every Windows Installer registration of `app` in turn, within the caller's queue
    /// turn. A failure says whether an earlier registration was already removed, so a downgrade
    /// knows the app is partly gone.
    fn remove_registrations(app: &SuiteApp) -> Result<(), msi_downgrade::Removal> {
        let installs = crate::detect::windows::msi_installs(app);
        if installs.is_empty() {
            return Err(msi_downgrade::Removal { error: format!("{} isn't registered with the Windows Installer", app.name), partly: false });
        }
        for (removed, m) in installs.iter().enumerate() {
            let status = process::command(msiexec())
                .args(["/x", &m.product_code, "/passive", "/norestart"])
                .status()
                .map_err(|e| format!("couldn't start the Windows Installer ({e})"))
                .and_then(|status| explain(status.code()));
            if let Err(error) = status {
                return Err(msi_downgrade::Removal { error, partly: removed > 0 });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder found only by the launch fallback, in another scope, never reaches the installer.
    #[cfg(windows)]
    #[test]
    fn an_update_takes_the_folder_only_from_the_records_own_scope() {
        let folder = Some(PathBuf::from(r"C:\Users\a\AppData\Local\Programs\Open3D\DeckTool"));
        assert_eq!(keep_if_in_scope(folder.clone(), true), folder);
        assert_eq!(keep_if_in_scope(folder, false), None);
        assert_eq!(keep_if_in_scope(None, true), None);
    }

    /// The default folder shown in the UI is the one a fresh install really uses.
    #[cfg(windows)]
    #[test]
    fn the_default_folder_is_the_arttool_suite_folder_in_program_files() {
        let paths = Paths::under(std::path::Path::new(r"C:\scratch"));
        let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap();
        assert_eq!(default_folder(PackageKind::Msi, Some(Arch::X64), &paths), Some(program_files.join("Open3D")));
        assert_eq!(default_folder(PackageKind::PortableZip, Some(Arch::X64), &paths), Some(paths.apps()));
        assert_eq!(default_folder(PackageKind::Dmg, Some(Arch::X64), &paths), None);
    }

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
    fn msi_updates_keep_the_registered_context_and_new_installs_send_no_scope() {
        // A fresh install passes nothing: the package's own default (per-user for a dual-scope
        // package) applies, and an older per-machine-only package keeps behaving exactly as
        // before.
        assert_eq!(msi_scope_arguments(None), Vec::<&str>::new());
        // A per-user update asks for the documented single-package per-user install...
        assert_eq!(msi_scope_arguments(Some(MsiScope::PerUser)), vec!["ALLUSERS=2", "MSIINSTALLPERUSER=1"]);
        // ...and a per-machine update asks for what per-machine-only packages already set
        // themselves, so it changes nothing for the current releases.
        assert_eq!(msi_scope_arguments(Some(MsiScope::PerMachine)), vec!["ALLUSERS=1"]);
    }

    #[test]
    fn each_way_back_is_allowed_only_for_the_install_it_can_undo() {
        let installed = |source| Installed {
            version: version::parse("0.5.0"),
            source,
            location: String::new(),
            package: None,
            externally_managed: false,
            origin: None,
            installation: None,
        };
        let (mac, msi, portable) = (installed(InstallSource::MacApp), installed(InstallSource::Msi), installed(InstallSource::Launcher));
        let (release, asset) = (Release::default(), Asset::default());
        let reinstall = Some(Downgrade::Reinstall { previous: &release, asset: &asset });
        let keep = Some(Downgrade::KeepOldCopy);
        let record = |kind| Record { version: "0.5.0".into(), kind, path: PathBuf::new(), installed_at: 0, files: Vec::new(), needs_root: false };
        let (appimage, tarball) = (record(PackageKind::AppImage), record(PackageKind::TarGz));
        // A macOS bundle over a Mac app, or an AppImage over one the launcher installed, keeps its
        // old copy until the new one is in place.
        assert!(keeps_old_copy(PackageKind::Dmg, Some(&mac), None));
        assert!(keeps_old_copy(PackageKind::AppImage, Some(&portable), Some(&appimage)));
        assert!(!keeps_old_copy(PackageKind::Dmg, Some(&portable), None));
        assert!(!keeps_old_copy(PackageKind::Dmg, None, None));
        assert!(!keeps_old_copy(PackageKind::Msi, Some(&mac), None));
        // An AppImage the launcher didn't record, a user's own AppImage, and a tarball don't.
        assert!(!keeps_old_copy(PackageKind::AppImage, Some(&portable), None));
        assert!(!keeps_old_copy(PackageKind::AppImage, Some(&installed(InstallSource::AppImage)), Some(&appimage)));
        assert!(!keeps_old_copy(PackageKind::AppImage, Some(&portable), Some(&tarball)));
        assert!(!keeps_old_copy(PackageKind::TarGz, Some(&portable), Some(&tarball)));
        // Each way back fits one kind of install; anything else is refused.
        assert!(downgrade_is_safe(None, PackageKind::AppImage, Some(&portable), None));
        assert!(downgrade_is_safe(keep, PackageKind::Dmg, Some(&mac), None));
        assert!(downgrade_is_safe(keep, PackageKind::AppImage, Some(&portable), Some(&appimage)));
        assert!(downgrade_is_safe(reinstall, PackageKind::Msi, Some(&msi), None));
        assert!(!downgrade_is_safe(keep, PackageKind::Msi, Some(&msi), None));
        assert!(!downgrade_is_safe(keep, PackageKind::Dmg, Some(&portable), None));
        assert!(!downgrade_is_safe(keep, PackageKind::AppImage, Some(&portable), None));
        assert!(!downgrade_is_safe(reinstall, PackageKind::Dmg, Some(&mac), None));
        assert!(!downgrade_is_safe(reinstall, PackageKind::AppImage, Some(&portable), Some(&appimage)));
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
