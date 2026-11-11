// SPDX-License-Identifier: GPL-3.0-or-later
//! Open3D Launcher updating itself.
//!
//! The new version is downloaded and verified like any Open3D app, then swapped in the way this
//! copy was installed. A small helper process waits for this launcher to exit, finishes anything
//! that can't happen while it runs (Windows locks a running exe), and starts the new one. The
//! launcher has to exit first anyway: it holds the single-instance lock, and a new copy started
//! while it runs would only ask it to show its window.
//!
//! | Installed as | Swap |
//! |---|---|
//! | macOS `.app` | copy the new bundle from the DMG over this one (fine while running), reopen it |
//! | Windows MSI (any folder, for one account or everyone) | after exit, run the new MSI in the copy's own context (everyone asks for permission), then start the exe it registers |
//! | Windows portable zip | after exit, unzip over this folder (settings in `Open3DLauncherData` stay) |
//! | Linux AppImage | rename the new AppImage over this one (fine while running), start it |
//! | Linux `.deb` / `.rpm` | the system package manager, with the password prompt; then restart |
//! | Linux Flatpak | reinstall the bundle on the same installation (per-user, or system-wide through `pkexec`); then restart |
//!
//! Anything else (FreeBSD, a development build) gets the download link instead.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::PackageKind;
use crate::app::App;
use crate::platform::{Platform, pick_asset};
use crate::protocol::JobPhase;
use crate::release::Release;
use crate::version::{self, Version};

use crate::http::{Http, Stop};
use crate::install::{self, Job};
use crate::paths::Paths;
use crate::process;

/// How this running launcher was installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Install {
    /// The `Open3D Launcher.app` bundle.
    MacApp(PathBuf),
    /// The per-user Windows Installer package; the exe.
    Msi(PathBuf),
    /// The Windows portable zip; the exe (with `portable.txt` beside it).
    Portable(PathBuf),
    /// The AppImage file.
    AppImage(PathBuf),
    /// A Debian or RPM package; the binary.
    Package(PackageKind, PathBuf),
    /// A Flatpak application.
    Flatpak,
    /// A standalone executable or unpacked tree.
    Standalone(PathBuf),
}

impl Install {
    fn kind(&self) -> PackageKind {
        match self {
            Self::MacApp(_) => PackageKind::Dmg,
            Self::Msi(_) => PackageKind::Msi,
            Self::Portable(_) => PackageKind::PortableZip,
            Self::AppImage(_) => PackageKind::AppImage,
            Self::Package(kind, _) => *kind,
            Self::Flatpak => PackageKind::Flatpak,
            Self::Standalone(_) => {
                #[cfg(windows)]
                {
                    PackageKind::PortableZip
                }
            }
        }
    }
}

/// Makes an installed copy believe it's this version, to try the self-update against the current
/// release. The restart helper drops it, so it lasts one run.
pub const PRETEND_VAR: &str = "OPEN3D_LAUNCHER_PRETEND_VERSION";

/// Makes a development build believe it's an installed copy, to try the self-update.
pub const DEV_VAR: &str = "CRAFT_UPDATE_DEV";

/// The version this app counts as (see [`PRETEND_VAR`]).
pub fn current_version(app: &App, fallback_pkg_version: &str) -> Option<Version> {
    let app_pretend = pretend_var(app);
    std::env::var(&app_pretend)
        .or_else(|_| std::env::var(PRETEND_VAR))
        .ok()
        .and_then(|v| version::parse(&v))
        .or_else(|| version::parse(fallback_pkg_version))
}

fn pretend_var(app: &App) -> String {
    format!("{}_PRETEND_VERSION", app.id.replace('-', "_").to_uppercase())
}

/// A pretend version lasts one run, including when an update schedules a restart.
fn clear_pretend_version(cmd: &mut std::process::Command, app: &App) {
    cmd.env_remove(PRETEND_VAR).env_remove(pretend_var(app));
}

/// How long a just-installed version stays exempt from being offered again.
const JUST_UPDATED_FOR: i64 = 24 * 3600;

/// Whether `release` is the version this app just updated itself to. Offering it again would
/// loop: update, restart, "update available", … That happens if a release's binary reports a
/// different version from its tag, or while a pretend version is set.
pub fn just_updated_to(paths: &Paths, release: &Release) -> bool {
    let Ok(text) = std::fs::read_to_string(marker(paths)) else { return false };
    let mut lines = text.lines();
    let (Some(tag), Some(at)) = (lines.next(), lines.next().and_then(|a| a.parse::<i64>().ok())) else { return false };
    tag == release.tag && crate::now().checked_sub(at).is_some_and(|age| (0..JUST_UPDATED_FOR).contains(&age))
}

fn marker(paths: &Paths) -> PathBuf {
    paths.data.join("self-update.txt")
}

/// How this copy is installed, if it's one the app knows how to replace.
pub fn current(app: &App) -> Option<Install> {
    let exe = std::env::current_exe().ok()?;
    current_at(app, &exe, std::env::var_os(DEV_VAR).is_some())
}

/// [`current`] for a known executable and opt-in setting, without reading the ambient
/// environment.
fn current_at(app: &App, exe: &Path, opted_in: bool) -> Option<Install> {
    let exe = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
    if is_development_build(&exe, opted_in) {
        return None;
    }
    detect(app, &exe)
}

/// Cargo's cache-directory marker at the root of a target directory.
const CARGO_CACHE_MARKER: &str = "CACHEDIR.TAG";

/// Whether `exe` runs from a Cargo build tree, wherever `CARGO_TARGET_DIR` or
/// `build.target-dir` put it. Normally Cargo caches rustc information in `.rustc_info.json` at
/// the target root, and that marker alone identifies the tree; a folder that installers put the
/// app in doesn't carry it, so this doesn't depend on the directory being called `target`. But
/// the cache can be disabled (`CARGO_CACHE_RUSTC_INFO=0`) or a tree copied without the hidden
/// file, so a profile output directory is also accepted when it has Cargo's `.fingerprint`,
/// `deps` and `build` subdirectories and its target root carries `CACHEDIR.TAG`.
/// `CACHEDIR.TAG` alone isn't Cargo-specific (the same signature marks any cache directory), so
/// it only corroborates that profile layout.
fn is_development_build(exe: &Path, opted_in: bool) -> bool {
    !opted_in && exe.ancestors().skip(1).any(is_cargo_build_dir)
}

/// Whether `dir` is a Cargo target root (by its rustc-info cache marker) or a profile output
/// directory inside one (by the cache-disabled fallback).
fn is_cargo_build_dir(dir: &Path) -> bool {
    if dir.join(".rustc_info.json").is_file() {
        return true;
    }
    let Some(profile_parent) = dir.parent() else { return false };
    let is_profile = [".fingerprint", "deps", "build"].into_iter().all(|name| dir.join(name).is_dir());
    if !is_profile {
        return false;
    }
    // The target root is the profile's parent, or its grandparent for cross-compiled output
    // (`<target>/<triple>/<profile>`). It must carry Cargo's marker, as a file rather than a
    // directory of that name.
    if profile_parent.join(CARGO_CACHE_MARKER).is_file() {
        return true;
    }
    profile_parent.parent().is_some_and(|root| root.join(CARGO_CACHE_MARKER).is_file())
}

/// Whether this installed copy can update itself.
pub fn is_self_updating(app: &App) -> bool {
    current(app).is_some()
}

/// Whether `exe` sits in a folder a Windows package manager owns (scoop, winget, Chocolatey, the
/// Store).
fn windows_package_manager_owns(exe: &Path) -> bool {
    // Compared as text so the rule reads the same wherever it is tested: either separator, any case.
    let text = exe.to_string_lossy().to_lowercase();
    let parts: Vec<&str> = text.split(['\\', '/']).collect();
    parts.contains(&"windowsapps")
        || parts.windows(2).any(|w| matches!((w[0], w[1]), ("scoop", "apps" | "shims") | ("winget", "packages") | ("chocolatey", "lib" | "bin")))
}

/// How the Windows copy at `exe` was installed: portable (a `portable.txt` beside it), by its MSI
/// (registered in either context, or the launcher's own per-user package), else a loose folder.
#[cfg(windows)]
fn detect(app: &App, exe: &Path) -> Option<Install> {
    let dir = exe.parent()?;
    if dir.join("portable.txt").is_file() {
        return Some(Install::Portable(exe.to_path_buf()));
    }
    // Any folder, either context: the MSI's own registration says so (see `msi_copy`). A copy
    // installed for everyone must never be unpacked over as if it were a loose folder.
    if crate::msi_copy::windows::find(app, exe).is_some() {
        return Some(Install::Msi(exe.to_path_buf()));
    }
    // scoop, winget, Chocolatey and the Store own their copies; an in-place replace leaves their record stale
    if windows_package_manager_owns(exe) {
        return None;
    }
    if exe.is_file() {
        return Some(Install::Standalone(exe.to_path_buf()));
    }
    None
}

/// What `exe`'s MSI update does (update in place, or move a per-user copy to everyone), or why it
/// stops. Asked while the app still runs (so it can show the error) and again by the helper.
#[cfg(windows)]
fn msi_step(app: &App, exe: &Path) -> Result<crate::msi_migrate::Step, String> {
    crate::msi_migrate::windows::step(app, exe)
}

/// Whether the helper starts the app after the update step: never while the old app is still
/// running (a second copy of it would open), always after a successful step, and after a failed
/// one only while its exe is still there.
fn restarts(app_exited: bool, step_succeeded: bool, exe_exists: bool) -> bool {
    app_exited && (step_succeeded || exe_exists)
}

/// The exe to start after an MSI update of a registered copy: the one its registrations name now
/// (the package may have moved an exact old default folder), else `exe`.
#[cfg(windows)]
fn exe_after_msi(app: &App, registered: bool, exe: &Path) -> PathBuf {
    registered
        .then(|| crate::msi_copy::registered_exe(&crate::msi_copy::windows::registered_exes(app)))
        .flatten()
        .filter(|moved| is_own_executable(app, moved))
        .unwrap_or_else(|| exe.to_path_buf())
}

/// Downloads `release` for `platform`, puts it in place and starts the helper that relaunches
/// it once this process has exited. On success the caller must quit promptly. `last` is called
/// once the download is ready and returns when the app may replace itself and restart.
#[allow(clippy::too_many_arguments)]
pub fn update(
    app: &App,
    install: &Install,
    release: &Release,
    platform: Platform,
    http: &Http,
    paths: &Paths,
    stop: &Stop,
    phase: &mut dyn FnMut(JobPhase),
    last: &dyn Fn() -> Result<(), String>,
    running_version: Option<&str>,
) -> Result<(), String> {
    prepare_update(app, install, release, platform, http, paths, stop, phase, last, running_version)?.start()
}

/// What remains of an update after [`prepare_update`]: starting the helper, which waits for this
/// process to exit. On Windows the helper then installs the update and starts the new version;
/// on macOS and Linux the update is already in place and the helper starts the new version.
#[must_use = "the update only installs or restarts once the restart is started"]
pub struct Restart {
    helper: Option<Box<dyn FnOnce() -> Result<(), String> + Send>>,
}

impl Restart {
    /// `start` runs when the app closes.
    pub(crate) fn later(start: impl FnOnce() -> Result<(), String> + Send + 'static) -> Self {
        Self { helper: Some(Box::new(start)) }
    }

    /// Starts the helper. Call it when the app's exit is certain: the helper waits only a minute
    /// for the app to exit. On Windows it installs nothing when the app is still running after
    /// that; on macOS and Linux the Flatpak helper starts nothing then, and the other helpers
    /// start the new version anyway.
    pub fn start(self) -> Result<(), String> {
        self.helper.map_or(Ok(()), |start| start())
    }
}

impl std::fmt::Debug for Restart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Restart").field("pending", &self.helper.is_some()).finish()
    }
}

/// [`update`] without starting the helper: the caller starts the returned [`Restart`] when the
/// app's exit is certain. On macOS and Linux the update is in place when this returns.
#[allow(clippy::too_many_arguments)]
pub fn prepare_update(
    app: &App,
    install: &Install,
    release: &Release,
    platform: Platform,
    http: &Http,
    paths: &Paths,
    stop: &Stop,
    phase: &mut dyn FnMut(JobPhase),
    last: &dyn Fn() -> Result<(), String>,
    running_version: Option<&str>,
) -> Result<Restart, String> {
    let cur_v = running_version.and_then(|v| current_version(app, v));
    if let (Some(rel_v), Some(cur_v)) = (release.version.as_ref(), cur_v.as_ref())
        && !version::offers_update(rel_v, cur_v)
    {
        // Compare the effective version: an explicit pretend version also exercises installation.
        // Without that opt-in, a build from source requires a strictly higher release.
        return Err(format!("{} {} is not newer than currently running version {}", app.name, release.version_label(), cur_v));
    }
    let (asset, _) = pick_asset(release, &[app.id], platform, &[install.kind()])
        .ok_or_else(|| format!("{} {} has no {} for {}", app.name, release.version_label(), install.kind().label(), platform.label()))?;
    let asset = asset.clone();
    crate::install::bind_artifact_version(app, release, &asset)?;
    let mut job = Job { http, paths, stop, phase };
    let file = install::download(release, &asset, &mut job)?;
    #[cfg(windows)]
    windows_precheck(app, install, paths, &file)?;
    // The restart ends this process: nothing else may be under way.
    last()?;
    phase(JobPhase::Installing);
    // Written before the swap starts. On Windows the helper does the swap, so the record is
    // written when it starts (see `windows_helper`): it clears the record when it fails, which a
    // write after it started could undo.
    let pid = std::process::id();
    let scheduled = (|| -> Result<Restart, String> {
        match install {
            #[cfg(windows)]
            Install::Msi(exe) => windows_helper(app, paths, release, pid, exe, &msi_finish(&file)?),
            #[cfg(windows)]
            Install::Portable(exe) => {
                exe.parent().ok_or_else(|| format!("{} isn't in a folder", app.name))?;
                windows_helper(app, paths, release, pid, exe, &Finish::Portable { zip: file, tmp: paths.cache.join(PORTABLE_STAGING) })
            }
            #[cfg(windows)]
            Install::Standalone(exe) => {
                exe.parent().ok_or_else(|| format!("{} isn't in a folder", app.name))?;
                windows_helper(app, paths, release, pid, exe, &Finish::Portable { zip: file, tmp: paths.cache.join(PORTABLE_STAGING) })
            }
            #[allow(unreachable_patterns)]
            _ => {
                let _ = (pid, &file);
                Err(format!("this copy of {} can't update itself; download the new version instead", app.name))
            }
        }
    })();
    let scheduled = unmark_unless_scheduled(paths, scheduled);
    // On macOS and Linux the update is in place once it is prepared.
    scheduled
}

/// Records `release` as just installed, so it isn't offered again for [`JUST_UPDATED_FOR`]: its
/// tag, the time, and the version it installs. The outcome records of an earlier update are
/// removed: this update writes its own.
fn mark_installed(paths: &Paths, release: &Release) {
    let _ = std::fs::remove_file(failure_file(paths));
    let _ = std::fs::remove_file(done_file(paths));
    let text = format!("{}\n{}\n{}\n", release.tag, crate::now(), release.version_label());
    if let Err(e) = crate::paths::write_atomic(&marker(paths), text.as_bytes()) {
        log::warn!("couldn't record the self-update: {e}");
    }
}

/// Where a finished update records the version it installed, for the new version's first start.
fn done_file(paths: &Paths) -> PathBuf {
    paths.data.join("self-update-done.txt")
}

/// Records that the update to `version` has been installed.
fn record_done(paths: &Paths, version: &str) {
    if let Err(e) = crate::paths::write_atomic(&done_file(paths), version.as_bytes()) {
        log::warn!("couldn't record the finished self-update: {e}");
    }
}

/// The version the scheduled update installs, from the record [`mark_installed`] wrote: its third
/// line, or the tag on its first line in a record without one.
fn scheduled_version(paths: &Paths) -> Option<String> {
    let text = read_record(&marker(paths))?;
    let mut lines = text.lines();
    let tag = lines.next();
    let version = lines.nth(1).filter(|version| !version.trim().is_empty()).or(tag);
    version.map(str::trim).filter(|version| !version.is_empty()).map(str::to_owned)
}

/// The most of an outcome record that is read: a reason longer than this is cut short.
const RECORD_LIMIT: usize = 4096;

/// The text of the record at `path`, at most [`RECORD_LIMIT`] bytes of it. `None` when it is
/// missing, unreadable or not a plain file (a link is not followed).
fn read_record(path: &Path) -> Option<String> {
    use std::io::Read;
    if !is_plain_file(path) {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(RECORD_LIMIT as u64 + 1).read_to_end(&mut bytes).ok()?;
    let cut = bytes.len() > RECORD_LIMIT;
    bytes.truncate(RECORD_LIMIT);
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if cut {
        // A multi-byte character split at the limit decodes as U+FFFD; drop it.
        text = text.trim_end_matches('\u{FFFD}').to_owned();
        text.push('…');
    }
    Some(text)
}

/// What the last self-update came to, read once by the next start ([`take_outcome`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The update installed this version, which is the one running now.
    Installed(Version),
    /// The update could not be installed, for this reason.
    Failed(String),
}

/// What the last self-update came to, once: the records are removed as they are read. A
/// recorded failure wins over a record of a finished update. A finished update counts only when
/// the version it installed is `running`, the version this copy counts as.
pub fn take_outcome(paths: &Paths, running: &str) -> Option<Outcome> {
    let failure = take_failure(paths);
    let done = read_record(&done_file(paths));
    let _ = std::fs::remove_file(done_file(paths));
    if let Some(reason) = failure {
        let reason = reason.trim();
        let reason = if reason.is_empty() { "no reason was recorded" } else { reason };
        return Some(Outcome::Failed(reason.to_owned()));
    }
    let installed = version::parse(done?.trim())?;
    (Some(&installed) == version::parse(running).as_ref()).then_some(Outcome::Installed(installed))
}

/// Removes the record of [`mark_installed`] when the swap couldn't be scheduled, so the update
/// stays on offer.
fn unmark_unless_scheduled<T>(paths: &Paths, scheduled: Result<T, String>) -> Result<T, String> {
    if scheduled.is_err() {
        let _ = std::fs::remove_file(marker(paths));
    }
    scheduled
}

/// A regular file, not a link to one.
fn is_plain_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

/// The first argument of the Windows update helper: a copy of this program that finishes an
/// update once the launcher has exited (see [`finish`]).
pub const FINISH_FLAG: &str = "--finish-update";

/// What the update helper does once the old launcher has exited.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Finish {
    /// Run the new MSI, which reinstalls into the same folder.
    Msi(PathBuf),
    /// Unpack the new portable zip into `tmp` and copy it over the launcher's folder.
    Portable { zip: PathBuf, tmp: PathBuf },
}

/// The helper's arguments: [`FINISH_FLAG`], the process id to wait for, the exe to start
/// afterwards, then the step.
fn helper_args(pid: u32, exe: &Path, finish: &Finish) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![FINISH_FLAG.into(), pid.to_string().into(), exe.into()];
    match finish {
        Finish::Msi(msi) => args.extend(["msi".into(), msi.into()]),
        Finish::Portable { zip, tmp } => args.extend(["portable".into(), zip.into(), tmp.into()]),
    }
    args
}

/// Reads [`helper_args`] back.
fn parse_helper_args(args: &[OsString]) -> Option<(u32, PathBuf, Finish)> {
    let [flag, pid, exe, step @ ..] = args else { return None };
    if flag != FINISH_FLAG {
        return None;
    }
    let pid = pid.to_str()?.parse().ok()?;
    let finish = match step {
        [kind, msi] if kind == "msi" => Finish::Msi(msi.into()),
        [kind, zip, tmp] if kind == "portable" => Finish::Portable { zip: zip.into(), tmp: tmp.into() },
        _ => return None,
    };
    Some((pid, exe.into(), finish))
}

fn msi_finish(file: &Path) -> Result<Finish, String> {
    install::msi_path(file).map(Finish::Msi)
}

/// The cache folder the update helper is copied into.
const HELPER_DIR: &str = "launcher-update-helper";
/// The cache folder a portable update is unpacked into.
const PORTABLE_STAGING: &str = "launcher-update";

/// Copies this program into the cache as the update helper, which waits for `pid` to exit, does
/// `finish` and starts `exe`. Run from the cache, the helper doesn't hold the installed exe that
/// the step replaces. The returned [`Restart`] records `release` as installed and starts the
/// helper.
#[cfg(windows)]
fn windows_helper(app: &App, paths: &Paths, release: &Release, pid: u32, exe: &Path, finish: &Finish) -> Result<Restart, String> {
    let program = std::env::current_exe().map_err(|e| format!("couldn't find this program ({e})"))?;
    let dir = paths.cache.join(HELPER_DIR);
    // The previous update's copy, when it's still there.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't schedule the update ({e})"))?;
    let helper = dir.join(program.file_name().ok_or("this program has no file name")?);
    std::fs::copy(&program, &helper).map_err(|e| format!("couldn't schedule the update ({e})"))?;
    let mut cmd = process::command(&helper);
    cmd.args(helper_args(pid, exe, finish));
    clear_pretend_version(&mut cmd, app);
    let (paths, release) = (paths.clone(), release.clone());
    Ok(Restart::later(move || {
        mark_installed(&paths, &release);
        let started = process::spawn_detached(cmd).map_err(|e| format!("couldn't schedule the update ({e})"));
        unmark_unless_scheduled(&paths, started)
    }))
}

/// Finishes an update when this program was started as the update helper; `args` are its
/// arguments without the program name. `None` when they don't start with [`FINISH_FLAG`]. The
/// app is started again even when the step fails: the old copy is then still in place.
#[cfg(windows)]
pub fn finish(app: &App, args: &[OsString]) -> Option<Result<(), String>> {
    if args.first().is_none_or(|a| a != FINISH_FLAG) {
        return None;
    }
    let Some((pid, exe, step)) = parse_helper_args(args) else { return Some(Err("the update helper's arguments are incomplete".into())) };
    let Ok(me) = std::env::current_exe() else { return Some(Err("the update helper couldn't find itself".into())) };
    let paths = match Paths::for_exe(app, &exe) {
        Ok(paths) => paths,
        Err(e) => return Some(Err(e)),
    };
    // Nothing runs unless this is the app's own update, signed like this helper.
    if let Err(e) = check_helper(app, &me, &paths, &exe, &step).and_then(|()| signed_like(app, &me, &exe)) {
        record_failure(&paths, &e);
        return Some(Err(e));
    }
    // Read before the step: the update may move or replace the old exe.
    let registered =
        matches!(step, Finish::Msi(_)) && matches!(crate::msi_copy::windows::find(app, &exe), Some(crate::msi_copy::MsiCopy::Registered { .. }));
    let wait = |pid| process::wait_for_exit(pid, std::time::Duration::from_secs(60));
    let start = |exe: &Path| {
        signed_like(app, &me, exe)
            .and_then(|()| process::spawn_detached(std::process::Command::new(exe)).map_err(|e| format!("couldn't start {} ({e})", exe.display())))
    };
    Some(finish_step(app, &me, &paths, pid, exe, &step, registered, &wait, &start))
}

/// The helper's work once its checks have passed: waits for `pid` with `wait`, does `step`, and
/// starts the app with `start` when [`restarts`] allows it. Every failure is recorded for the
/// app's next start.
#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn finish_step(
    app: &App,
    reference: &Path,
    paths: &Paths,
    pid: u32,
    exe: PathBuf,
    step: &Finish,
    registered: bool,
    wait: &dyn Fn(u32) -> bool,
    start: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let portable_before = has_portable_marker(&exe);
    let stepped = step_after_wait(app, reference, pid, &exe, step, wait);
    let done = stepped.result;
    if let Err(e) = &done {
        record_failure(paths, e);
    }
    // The new app, or the old one when the step failed.
    let exe = if done.is_ok() { exe_after_msi(app, registered, &exe) } else { exe };
    // A standalone copy updated from the portable zip gains `portable.txt` and keeps its folders
    // beside it from now on: the records the new app reads go there.
    let version = scheduled_version(paths);
    let moved = (done.is_ok() && has_portable_marker(&exe) != portable_before).then(|| Paths::for_exe(app, &exe).ok()).flatten();
    if let Some(after) = &moved {
        move_marker(paths, after);
    }
    let paths = moved.as_ref().unwrap_or(paths);
    // An app that is still running gets no second copy. A failed move can leave no program to
    // start; its recorded error already says how to get the app back, and a restart error must
    // not replace it.
    if !restarts(stepped.exited, done.is_ok(), exe.is_file()) {
        return done;
    }
    // Written before the new app starts, so its first start finds it. A failed start removes it.
    if done.is_ok()
        && let Some(version) = &version
    {
        record_done(paths, version);
    }
    let started = start(&exe);
    let restarted = started.is_ok();
    let result = with_restart(done, started);
    if !restarted && let Err(e) = &result {
        record_failure(paths, e);
    }
    result
}

/// The helper's result once it has tried to start the app: when both the update step and the
/// start fail, the error names both, the step's first.
fn with_restart(done: Result<(), String>, started: Result<(), String>) -> Result<(), String> {
    match (done, started) {
        (done, Ok(())) => done,
        (Ok(()), Err(restart)) => Err(restart),
        (Err(update), Err(restart)) => Err(format!("{update}; {restart}")),
    }
}

/// Whether `exe` has `portable.txt` beside it, which makes [`Paths::for_exe`] keep its folders
/// there.
#[cfg(windows)]
fn has_portable_marker(exe: &Path) -> bool {
    exe.parent().is_some_and(|dir| dir.join("portable.txt").is_file())
}

/// Moves the record of the scheduled update from `from` to `to`, the folders the updated app
/// reads, and removes outcome records left in `to` by an earlier update.
#[cfg(windows)]
fn move_marker(from: &Paths, to: &Paths) {
    let _ = std::fs::remove_file(failure_file(to));
    let _ = std::fs::remove_file(done_file(to));
    if let Some(text) = read_record(&marker(from))
        && crate::paths::write_atomic(&marker(to), text.as_bytes()).is_ok()
    {
        let _ = std::fs::remove_file(marker(from));
    }
}

/// Where a failed update's error waits for the next start.
fn failure_file(paths: &Paths) -> PathBuf {
    paths.logs().join("self-update-error.txt")
}

/// Records a failed update: the next check offers the update again, and the next start shows
/// `error`. A record of the update as finished is removed.
pub(crate) fn record_failure(paths: &Paths, error: &str) {
    let _ = std::fs::remove_file(marker(paths));
    let _ = std::fs::remove_file(done_file(paths));
    let _ = std::fs::create_dir_all(paths.logs());
    let _ = crate::paths::write_atomic(&failure_file(paths), error.as_bytes());
}

/// The error of the last update, if it failed, once. A long error is cut short at
/// [`RECORD_LIMIT`] bytes.
pub fn take_failure(paths: &Paths) -> Option<String> {
    let file = failure_file(paths);
    let error = read_record(&file);
    if is_plain_file(&file) {
        let _ = std::fs::remove_file(&file);
    }
    error
}

/// Whether `exe` is named like this app's own executable (case-insensitive, any of its ids).
#[cfg(windows)]
fn is_own_executable(app: &App, exe: &Path) -> bool {
    exe.file_stem().and_then(|stem| stem.to_str()).is_some_and(|stem| app.stems().any(|id| id.eq_ignore_ascii_case(stem)))
}

/// Refuses an executable that isn't this app's own, as the helper would.
#[cfg(windows)]
fn own_executable(app: &App, exe: &Path) -> Result<(), String> {
    if is_own_executable(app, exe) { Ok(()) } else { Err(format!("the update helper only finishes {}'s own update", app.name)) }
}

/// Checks, while the app still runs and can show the error, what the helper checks before
/// it installs: this program, the installed exe and the new MSI are signed by one publisher (when
/// this program is signed at all, see [`signed_like`]), and
/// the helper finds the cache in `paths` that holds its copy, the download and the staging folder.
#[cfg(windows)]
fn windows_precheck(app: &App, install: &Install, paths: &Paths, download: &Path) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| format!("couldn't find this program ({e})"))?;
    match install {
        Install::Msi(exe) => signed_like(app, &me, exe)
            .and_then(|()| msi_step(app, exe).map(drop))
            .and_then(|()| signed_like(app, &me, download))
            .and_then(|()| own_executable(app, exe))
            .and_then(|()| same_cache_as_helper(app, paths, exe)),
        Install::Portable(exe) | Install::Standalone(exe) => {
            signed_like(app, &me, exe).and_then(|()| own_executable(app, exe)).and_then(|()| same_cache_as_helper(app, paths, exe))
        }
        _ => Ok(()),
    }
}

/// Accepts `paths` when the update helper, which finds the app's folders again from `exe`
/// ([`Paths::for_exe`]), finds the same cache. Refuses an app that keeps its files in the
/// temporary folder because its own folders couldn't be found.
#[cfg(windows)]
fn same_cache_as_helper(app: &App, paths: &Paths, exe: &Path) -> Result<(), String> {
    let canonical = |p: &Path| p.canonicalize().ok();
    let theirs = Paths::for_exe(app, exe).ok().and_then(|helper| canonical(&helper.cache));
    if theirs.is_some() && theirs == canonical(&paths.cache) {
        Ok(())
    } else {
        Err(format!("{} can't update itself while it keeps its files in {}; download the new version instead", app.name, paths.cache.display()))
    }
}

/// Accepts `file` when it carries a valid Authenticode signature from the same publisher as
/// `reference`, this program, and refuses an unsigned file or another publisher's.
///
/// Open3D publishes unsigned builds (it has no code-signing certificate). When `reference`
/// itself isn't signed there is no publisher to compare, so nothing is refused here: the
/// download's published SHA-256, which [`install::download`] checks and without which it
/// refuses the release, is what vouches for the file. A signed copy keeps the full check.
#[cfg(windows)]
fn signed_like(app: &App, reference: &Path, file: &Path) -> Result<(), String> {
    let publisher = |path: &Path| verifysign::verify_file(path).map(|signature| signature.subject_name());
    let Ok(ours) = publisher(reference) else { return Ok(()) };
    let theirs = publisher(file).map_err(|e| format!("{} isn't signed ({e}), unlike this copy of {}", file.display(), app.name))?;
    if ours == theirs { Ok(()) } else { Err(format!("{} isn't signed by {}'s publisher", file.display(), app.name)) }
}

/// Whether `file` is a Windows program or library (it starts with the `MZ` header), whatever its
/// name.
#[cfg(windows)]
fn is_program(file: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 2];
    std::fs::File::open(file).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && magic == *b"MZ"
}

/// Allows only the app's own update: `exe` is this app's MSI or portable installation,
/// of the kind `step` updates, and the helper (`me`), the download and the staging folder are in
/// that installation's cache. Anything else is refused before the helper runs or starts a file.
#[cfg(windows)]
fn check_helper(app: &App, me: &Path, paths: &Paths, exe: &Path, step: &Finish) -> Result<(), String> {
    let refused = || format!("the update helper only finishes {}'s own update", app.name);
    let canonical = |p: &Path| p.canonicalize().map_err(|_| refused());
    let cache = canonical(&paths.cache)?;
    if canonical(me.parent().ok_or_else(refused)?)? != cache.join(HELPER_DIR) {
        return Err(refused());
    }
    let exe = canonical(exe)?;
    // Whatever the folder, the program to restart is this app's own, never another executable.
    if !is_own_executable(app, &exe) {
        return Err(refused());
    }
    let downloads = canonical(&paths.downloads())?;
    let in_downloads = |file: &Path| canonical(file).map(|f| f.parent() == Some(downloads.as_path()));
    let allowed = match (detect(app, &exe), step) {
        (Some(Install::Msi(installed)), Finish::Msi(msi)) => installed == exe && in_downloads(msi)?,
        (Some(Install::Portable(installed) | Install::Standalone(installed)), Finish::Portable { zip, tmp }) => {
            installed == exe
                && in_downloads(zip)?
                && tmp.file_name().is_some_and(|n| n == PORTABLE_STAGING)
                && canonical(tmp.parent().ok_or_else(refused)?)? == cache
        }
        _ => false,
    };
    if allowed { Ok(()) } else { Err(refused()) }
}

/// [`step_after_wait`], waiting up to `wait` for `pid` to exit.
#[cfg(all(windows, test))]
fn run_step(app: &App, reference: &Path, pid: u32, exe: &Path, step: &Finish, wait: std::time::Duration) -> Result<(), String> {
    step_after_wait(app, reference, pid, exe, step, &|pid| process::wait_for_exit(pid, wait)).result
}

/// What [`step_after_wait`] came to.
#[cfg(windows)]
struct Stepped {
    /// The old app had exited when the wait returned.
    exited: bool,
    result: Result<(), String>,
}

/// Calls `wait`, which says whether `pid` exited in time, does `step` once it has, and removes
/// the download either way. The MSI, and every program in the portable zip, must be signed like
/// `reference`.
#[cfg(windows)]
fn step_after_wait(app: &App, reference: &Path, pid: u32, exe: &Path, step: &Finish, wait: &dyn Fn(u32) -> bool) -> Stepped {
    let waited = wait(pid);
    let result = if waited {
        match step {
            Finish::Msi(msi) => {
                signed_like(app, reference, msi).and_then(|()| msi_step(app, exe)).and_then(|step| crate::msi_migrate::windows::run(msi, &step))
            }
            Finish::Portable { zip, tmp } => match (exe.parent(), exe.file_name()) {
                (Some(dir), Some(executable)) => crate::portable::unpack_over(zip, tmp, dir, executable, &|file| {
                    // The expected executable must be signed even when its contents aren't a PE file.
                    if file.file_name() == Some(executable) || is_program(file) { signed_like(app, reference, file) } else { Ok(()) }
                }),
                _ => Err(format!("{} isn't in a folder", app.name)),
            },
        }
    } else {
        Err(format!("{} was still running after a minute, so the update wasn't installed", app.name))
    };
    match step {
        Finish::Msi(msi) => {
            let _ = std::fs::remove_file(msi);
        }
        // `unpack_over` removes the staging folder, unless it holds files it couldn't put back.
        Finish::Portable { zip, .. } => {
            let _ = std::fs::remove_file(zip);
        }
    }
    Stepped { exited: waited, result }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_package_manager_folders_are_recognised() {
        for owned in [
            r"C:\Users\Ada\scoop\apps\open3d-launcher\current\open3d-launcher.exe",
            r"C:\Users\Ada\scoop\shims\open3d-launcher.exe",
            r"C:\ProgramData\scoop\apps\open3d-launcher\current\open3d-launcher.exe",
            r"C:\Users\Ada\AppData\Local\Microsoft\WinGet\Packages\Storytold.Open3DLauncher_x\open3d-launcher.exe",
            r"C:\ProgramData\chocolatey\lib\open3d-launcher\tools\open3d-launcher.exe",
            r"C:\Program Files\WindowsApps\Storytold.Open3DLauncher_1.0_x64\open3d-launcher.exe",
        ] {
            assert!(windows_package_manager_owns(Path::new(owned)), "{owned}");
        }
        for loose in
            [r"C:\Tools\Open3D Launcher\open3d-launcher.exe", r"D:\myscoopy\apps\open3d-launcher.exe", r"C:\Users\Ada\Downloads\open3d-launcher.exe"]
        {
            assert!(!windows_package_manager_owns(Path::new(loose)), "{loose}");
        }
    }

    #[test]
    fn a_missing_msi_cannot_schedule_a_self_update() {
        let dir = tempfile::tempdir().unwrap();
        assert!(msi_finish(&dir.path().join("missing.msi")).is_err());
    }

    /// The helper reads back exactly what the launcher passed it, paths with spaces and
    /// apostrophes included, and nothing that is incomplete or unknown.
    #[test]
    fn the_update_helper_reads_back_its_arguments() {
        let exe = Path::new(r"C:\Users\O'Brien\AppData\Local\Programs\Open3D Launcher\open3d-launcher.exe");
        let msi = Finish::Msi(PathBuf::from(r"C:\Users\O'Brien\AppData\Local\Open3D Launcher\cache\open3d-launcher-0.7.0-windows-x64.msi"));
        let portable =
            Finish::Portable { zip: PathBuf::from(r"D:\Tools\x 1.zip"), tmp: PathBuf::from(r"D:\Tools\Open3DLauncherData\cache\launcher-update") };
        for finish in [msi, portable] {
            let args = helper_args(4242, exe, &finish);
            assert_eq!(args.first().map(OsString::as_os_str), Some(std::ffi::OsStr::new(FINISH_FLAG)));
            assert_eq!(parse_helper_args(&args), Some((4242, exe.to_path_buf(), finish)));
        }
        let os = |args: &[&str]| args.iter().map(OsString::from).collect::<Vec<_>>();
        for bad in [
            os(&[]),
            os(&["--hidden"]),
            os(&[FINISH_FLAG, "4242", "launcher.exe"]),
            os(&[FINISH_FLAG, "4242", "launcher.exe", "msi"]),
            os(&[FINISH_FLAG, "4242", "launcher.exe", "msi", "a.msi", "extra"]),
            os(&[FINISH_FLAG, "4242", "launcher.exe", "portable", "a.zip"]),
            os(&[FINISH_FLAG, "not a pid", "launcher.exe", "msi", "a.msi"]),
            os(&[FINISH_FLAG, "4242", "launcher.exe", "script", "Remove-Item C:\\"]),
            os(&["--finish", "4242", "launcher.exe", "msi", "a.msi"]),
        ] {
            assert_eq!(parse_helper_args(&bad), None, "{bad:?}");
        }
    }

    /// Arguments that aren't the helper's leave the launcher to start as usual.
    #[cfg(windows)]
    #[test]
    fn only_the_helper_flag_starts_the_update_helper() {
        assert_eq!(finish(&crate::suite::LAUNCHER, &[]), None);
        assert_eq!(finish(&crate::suite::LAUNCHER, &[OsString::from("--hidden")]), None);
        assert!(finish(&crate::suite::LAUNCHER, &[OsString::from(FINISH_FLAG)]).is_some_and(|done| done.is_err()));
    }

    /// The portable update waits for the old launcher to exit, then replaces its files with the
    /// zip's folder, keeps the settings beside it, and removes the download and the staging folder.
    #[cfg(windows)]
    #[test]
    fn a_portable_update_replaces_the_launcher_after_it_exits() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Open3D Launcher");
        std::fs::create_dir_all(folder.join("Open3DLauncherData")).unwrap();
        std::fs::write(folder.join("open3d-launcher.exe"), "0.1.0").unwrap();
        std::fs::write(folder.join("portable.txt"), "").unwrap();
        std::fs::write(folder.join("Open3DLauncherData").join("settings.json"), "{}").unwrap();
        let zip = dir.path().join("open3d-launcher-0.2.0-windows-x64-portable.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&zip).unwrap());
        let signed_exe = std::fs::read(system_program("cmd.exe")).unwrap();
        for (name, body) in [
            ("open3d-launcher-0.2.0-windows-x64-portable/open3d-launcher.exe", signed_exe.as_slice()),
            ("open3d-launcher-0.2.0-windows-x64-portable/portable.txt", b"".as_slice()),
        ] {
            writer.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap();
        let tmp = dir.path().join("cache").join("launcher-update");
        let step = Finish::Portable { zip: zip.clone(), tmp: tmp.clone() };

        let mut old = BlockedLauncher::start();
        let exe = folder.join("open3d-launcher.exe");
        std::thread::scope(|scope| {
            // Inside the scope, so a failed assertion drops the gate and releases the helper
            // before the scope joins it.
            let (gate, wait) = WaitGate::new();
            let (helper_exe, pid) = (exe.clone(), old.id());
            let helper = scope.spawn(move || step_after_wait(&crate::suite::LAUNCHER, &system_program("cmd.exe"), pid, &helper_exe, &step, &wait));
            gate.entered();
            old.assert_running("while the helper was waiting");
            assert_eq!(std::fs::read(&exe).unwrap(), b"0.1.0", "replaced while the old launcher was still running");
            old.finish().expect("the stand-in launcher couldn't be reaped");
            gate.release();
            let stepped = helper.join().unwrap();
            assert!(stepped.exited, "the wait reported that the launcher was still running although it had exited");
            stepped.result.unwrap();
        });
        assert_eq!(std::fs::read(&exe).unwrap(), signed_exe);
        assert_eq!(std::fs::read_to_string(folder.join("Open3DLauncherData").join("settings.json")).unwrap(), "{}");
        assert!(!folder.join("open3d-launcher-0.2.0-windows-x64-portable").exists(), "the zip's folder must not be nested in the launcher's");
        assert!(!zip.exists() && !tmp.exists(), "the download and the staging folder are removed");
    }

    /// Stands in for the old launcher: `sort.exe` keeps running until the test closes its stdin
    /// pipe, so the helper's wait cannot succeed early.
    #[cfg(windows)]
    struct BlockedLauncher {
        child: Option<std::process::Child>,
    }

    #[cfg(windows)]
    impl BlockedLauncher {
        fn start() -> Self {
            let child = std::process::Command::new(system_program("sort.exe"))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            Self { child: Some(child) }
        }

        fn id(&self) -> u32 {
            self.child.as_ref().unwrap().id()
        }

        /// Fails unless the stand-in is still running. `try_wait` keeps the handle owned, so
        /// `finish` and `Drop` can still reap it.
        fn assert_running(&mut self, when: &str) {
            let child = self.child.as_mut().unwrap();
            assert!(child.try_wait().unwrap().is_none(), "the stand-in launcher had already exited {when}");
        }

        /// Closes the pipe so `sort.exe` exits, and reaps it. If reaping fails the child stays
        /// owned by `self`, so `Drop` can still kill and reap it.
        fn finish(&mut self) -> std::io::Result<std::process::ExitStatus> {
            let child = self.child.as_mut().unwrap();
            drop(child.stdin.take());
            child.wait()
        }
    }

    #[cfg(windows)]
    impl Drop for BlockedLauncher {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                // Closing the pipe normally lets sort exit; killing is the fallback after a failed
                // assertion, and the wait reaps it either way.
                drop(child.stdin.take());
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    /// A wait for [`step_after_wait`] that the test controls: it reports when the helper has
    /// reached its wait, blocks until the test releases it, and then asks the real
    /// [`process::wait_for_exit`] whether the launcher has exited, without waiting.
    #[cfg(windows)]
    struct WaitGate {
        entered: std::sync::mpsc::Receiver<()>,
        release: std::sync::mpsc::Sender<()>,
    }

    #[cfg(windows)]
    impl WaitGate {
        /// The gate and the wait to hand to the helper. The wait holds the only sender of
        /// `entered`, so a helper that ends without reaching its wait fails [`WaitGate::entered`]
        /// instead of blocking it, and dropping the gate releases the wait.
        fn new() -> (Self, impl Fn(u32) -> bool + Send) {
            let (entered_tx, entered) = std::sync::mpsc::channel();
            let (release, released) = std::sync::mpsc::channel::<()>();
            let released = std::sync::Mutex::new(released);
            let wait = move |pid| {
                let _ = entered_tx.send(());
                let _ = released.lock().map(|released| released.recv());
                process::wait_for_exit(pid, std::time::Duration::ZERO)
            };
            (Self { entered, release }, wait)
        }

        /// Blocks until the helper has reached its wait.
        fn entered(&self) {
            self.entered.recv().expect("the helper never reached its wait");
        }

        /// Lets the helper's wait return.
        fn release(&self) {
            self.release.send(()).expect("the helper stopped before its wait was released");
        }
    }

    /// A launcher without `portable.txt` is a standalone installation, and the helper still
    /// replaces its own executable after it exits. The test holds a stand-in launcher alive
    /// through a pipe and holds the helper inside its wait until the stand-in has exited, so the
    /// executable must still be the old one while the helper waits, and the real
    /// [`process::wait_for_exit`] must report the live stand-in as running and the exited one as gone.
    #[cfg(windows)]
    #[test]
    fn a_standalone_update_replaces_the_launcher_after_it_exits() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Open3D Launcher");
        std::fs::create_dir_all(folder.join("Open3DLauncherData")).unwrap();
        let exe = folder.join("open3d-launcher.exe");
        std::fs::write(&exe, "0.1.0").unwrap();
        std::fs::write(folder.join("Open3DLauncherData").join("settings.json"), "{}").unwrap();
        let zip = dir.path().join("open3d-launcher-0.2.0-windows-x64-portable.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&zip).unwrap());
        let signed_exe = std::fs::read(system_program("cmd.exe")).unwrap();
        for (name, body) in [
            ("open3d-launcher-0.2.0-windows-x64-portable/open3d-launcher.exe", signed_exe.as_slice()),
            ("open3d-launcher-0.2.0-windows-x64-portable/portable.txt", b"".as_slice()),
        ] {
            writer.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap();
        let tmp = dir.path().join("cache").join(PORTABLE_STAGING);
        let step = Finish::Portable { zip: zip.clone(), tmp: tmp.clone() };

        let mut old = BlockedLauncher::start();
        let pid = old.id();
        std::thread::scope(|scope| {
            // Inside the scope, so a failed assertion drops the gate and releases the helper
            // before the scope joins it.
            let (gate, wait) = WaitGate::new();
            let helper_exe = exe.clone();
            let helper = scope.spawn(move || step_after_wait(&crate::suite::LAUNCHER, &system_program("cmd.exe"), pid, &helper_exe, &step, &wait));
            gate.entered();
            old.assert_running("while the helper was waiting");
            assert_eq!(std::fs::read(&exe).unwrap(), b"0.1.0", "replaced while the old launcher was still running");
            assert!(!process::wait_for_exit(pid, std::time::Duration::ZERO), "the wait reported the running launcher as exited");
            old.finish().expect("the stand-in launcher couldn't be reaped");
            gate.release();
            let stepped = helper.join().unwrap();
            assert!(stepped.exited, "the wait reported that the launcher was still running although it had exited");
            stepped.result.unwrap();
        });
        assert_eq!(std::fs::read(&exe).unwrap(), signed_exe);
        assert_eq!(std::fs::read_to_string(folder.join("Open3DLauncherData").join("settings.json")).unwrap(), "{}");
        // The update's zip carries `portable.txt`, so the replaced copy is detected as portable
        // and keeps its data in the folder beside the executable.
        assert_eq!(detect(&crate::suite::LAUNCHER, &exe), Some(Install::Portable(exe.clone())));
        assert_eq!(Paths::for_exe(&crate::suite::LAUNCHER, &exe).unwrap().data, folder.join("Open3DLauncherData").join("data"));
        assert!(!folder.join("open3d-launcher-0.2.0-windows-x64-portable").exists(), "the zip's folder must not be nested in the launcher's");
        assert!(!zip.exists() && !tmp.exists(), "the download and the staging folder are removed");
    }

    /// The helper finishes only the launcher's own update: its own installation, with the
    /// download from its own cache. A signed launcher must not run or start anything else.
    #[cfg(windows)]
    #[test]
    fn the_update_helper_refuses_anything_but_the_launchers_own_update() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Open3D Launcher");
        std::fs::create_dir_all(&folder).unwrap();
        let exe = folder.join("open3d-launcher.exe");
        std::fs::write(&exe, "0.1.0").unwrap();
        std::fs::write(folder.join("portable.txt"), "").unwrap();
        let paths = Paths::under(dir.path());
        let cache = paths.cache.clone();
        std::fs::create_dir_all(paths.downloads()).unwrap();
        std::fs::create_dir_all(cache.join(HELPER_DIR)).unwrap();
        let me = cache.join(HELPER_DIR).join("open3d-launcher.exe");
        std::fs::write(&me, "helper").unwrap();
        let zip = paths.downloads().join("open3d-launcher-0.2.0-windows-x64-portable.zip");
        std::fs::write(&zip, "zip").unwrap();
        let elsewhere = dir.path().join("elsewhere.zip");
        std::fs::write(&elsewhere, "zip").unwrap();
        let other = dir.path().join("other.exe");
        std::fs::write(&other, "other").unwrap();
        let other_in_folder = folder.join("other.exe");
        std::fs::write(&other_in_folder, "other").unwrap();
        let other_app = folder.join(format!("{}.exe", crate::suite::OPENTOOLS.id));
        std::fs::write(&other_app, "another suite app").unwrap();
        let staging = cache.join(PORTABLE_STAGING);
        let portable = |zip: &Path, tmp: &Path| Finish::Portable { zip: zip.to_path_buf(), tmp: tmp.to_path_buf() };

        assert_eq!(check_helper(&crate::suite::LAUNCHER, &me, &paths, &exe, &portable(&zip, &staging)), Ok(()));
        // A helper that isn't in the installation's cache.
        assert!(check_helper(&crate::suite::LAUNCHER, &other, &paths, &exe, &portable(&zip, &staging)).is_err());
        // Not the launcher's installation.
        assert!(check_helper(&crate::suite::LAUNCHER, &me, &paths, &other, &portable(&zip, &staging)).is_err());
        // A portable marker doesn't make a foreign executable the launcher.
        assert!(check_helper(&crate::suite::LAUNCHER, &me, &paths, &other_in_folder, &portable(&zip, &staging)).is_err());
        // Another Open3D app's executable is not the launcher, even in the same folder.
        assert!(check_helper(&crate::suite::LAUNCHER, &me, &paths, &other_app, &portable(&zip, &staging)).is_err());
        // A download from outside the cache.
        assert!(check_helper(&crate::suite::LAUNCHER, &me, &paths, &exe, &portable(&elsewhere, &staging)).is_err());
        // Unpacking anywhere but the cache's staging folder.
        assert!(check_helper(&crate::suite::LAUNCHER, &me, &paths, &exe, &portable(&zip, &dir.path().join(PORTABLE_STAGING))).is_err());
        assert!(check_helper(&crate::suite::LAUNCHER, &me, &paths, &exe, &portable(&zip, &paths.downloads())).is_err());
        // An MSI for a portable installation.
        assert!(check_helper(&crate::suite::LAUNCHER, &me, &paths, &exe, &Finish::Msi(zip.clone())).is_err());
        // Without portable.txt it is a standalone executable, which updates from the same zip.
        std::fs::remove_file(folder.join("portable.txt")).unwrap();
        assert_eq!(check_helper(&crate::suite::LAUNCHER, &me, &paths, &exe, &portable(&zip, &staging)), Ok(()));
        // A foreign executable is not a standalone launcher either.
        assert!(check_helper(&crate::suite::LAUNCHER, &me, &paths, &other_in_folder, &portable(&zip, &staging)).is_err());
        // An MSI is for an MSI installation only.
        assert!(check_helper(&crate::suite::LAUNCHER, &me, &paths, &exe, &Finish::Msi(zip.clone())).is_err());
    }

    /// The helper finds a portable launcher's folders beside it, as that launcher does.
    #[test]
    fn the_helper_uses_the_portable_launchers_own_folders() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("open3d-launcher.exe");
        std::fs::write(dir.path().join("portable.txt"), "").unwrap();
        let paths = Paths::for_exe(&crate::suite::LAUNCHER, &exe).unwrap();
        assert_eq!(paths.cache, dir.path().join("Open3DLauncherData").join("cache"));
        assert_eq!(paths.data, dir.path().join("Open3DLauncherData").join("data"));
    }

    /// The launcher quits for an update only when the helper finds the cache the update was
    /// downloaded into. One that keeps its files in the temporary folder, because its own folders
    /// couldn't be found, refuses before it quits: the helper would look in the per-user folders.
    #[cfg(windows)]
    #[test]
    fn a_launcher_whose_helper_looks_in_other_folders_does_not_quit_for_an_update() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Open3D Launcher");
        std::fs::create_dir_all(&folder).unwrap();
        let exe = folder.join("open3d-launcher.exe");
        std::fs::write(&exe, "0.1.0").unwrap();
        let own = Paths::under(&folder.join("Open3DLauncherData"));
        let temporary = Paths::under(&dir.path().join("Temp").join("open3d-launcher"));
        for paths in [&own, &temporary] {
            std::fs::create_dir_all(paths.downloads()).unwrap();
        }
        // Installed with the MSI: the helper calls `Paths::new` again.
        assert!(same_cache_as_helper(&crate::suite::LAUNCHER, &temporary, &exe).is_err_and(|e| e.contains("can't update itself")));
        std::fs::write(folder.join("portable.txt"), "").unwrap();
        assert_eq!(same_cache_as_helper(&crate::suite::LAUNCHER, &own, &exe), Ok(()));
        assert!(same_cache_as_helper(&crate::suite::LAUNCHER, &temporary, &exe).is_err());
    }

    /// An update whose swap couldn't be scheduled stays on offer, and a scheduled one is not
    /// offered again unless the helper, running alongside, records that it failed.
    #[test]
    fn only_a_scheduled_update_is_recorded_as_installed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(&paths.data).unwrap();
        let release = Release { tag: "v0.2.0".into(), ..Release::default() };
        mark_installed(&paths, &release);
        assert!(unmark_unless_scheduled(&paths, Err::<(), _>("couldn't schedule the update".into())).is_err());
        assert!(!just_updated_to(&paths, &release));
        mark_installed(&paths, &release);
        assert!(unmark_unless_scheduled(&paths, Ok(())).is_ok());
        assert!(just_updated_to(&paths, &release));
        // The helper refuses before this process has finished scheduling it.
        mark_installed(&paths, &release);
        record_failure(&paths, "the update helper only finishes Open3D Launcher's own update");
        assert!(unmark_unless_scheduled(&paths, Ok(())).is_ok());
        assert!(!just_updated_to(&paths, &release), "a failure the helper recorded must stay recorded");
    }

    /// A failed update is offered again and its error is logged at the next start, once.
    #[test]
    fn a_failed_update_is_offered_again_and_reported_once() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let release = Release { tag: "v0.2.0".into(), ..Release::default() };
        std::fs::create_dir_all(&paths.data).unwrap();
        crate::paths::write_atomic(&marker(&paths), format!("v0.2.0\n{}\n", crate::now()).as_bytes()).unwrap();
        assert!(just_updated_to(&paths, &release));
        record_failure(&paths, "the Windows Installer stopped with error 1603");
        assert!(!just_updated_to(&paths, &release));
        assert_eq!(take_failure(&paths).as_deref(), Some("the Windows Installer stopped with error 1603"));
        assert_eq!(take_failure(&paths), None);
    }

    /// The outcome is read once; a failure wins over a finished update, which counts only for the
    /// version it installed.
    #[test]
    fn the_outcome_of_an_update_is_read_once() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        assert_eq!(take_outcome(&paths, "0.2.0"), None);
        record_done(&paths, "v0.2.0");
        assert_eq!(take_outcome(&paths, "0.2.0"), Some(Outcome::Installed(Version::new(0, 2, 0))));
        assert_eq!(take_outcome(&paths, "0.2.0"), None, "read once");

        record_done(&paths, "v0.2.0");
        assert_eq!(take_outcome(&paths, "0.1.0"), None, "the old version still runs");
        assert!(!done_file(&paths).exists(), "a record that doesn't match is removed too");

        record_done(&paths, "v0.2.0");
        record_failure(&paths, "the Windows Installer stopped with error 1603");
        assert!(!done_file(&paths).exists(), "a failure removes the record of a finished update");
        record_done(&paths, "v0.2.0");
        assert_eq!(take_outcome(&paths, "0.2.0"), Some(Outcome::Failed("the Windows Installer stopped with error 1603".into())));
        assert_eq!(take_outcome(&paths, "0.2.0"), None);

        // Damaged records: no version, an empty reason, a reason longer than the limit.
        crate::paths::write_atomic(&done_file(&paths), b"\xff\xfe not a version").unwrap();
        assert_eq!(take_outcome(&paths, "0.2.0"), None);
        record_failure(&paths, "  \n");
        assert_eq!(take_outcome(&paths, "0.2.0"), Some(Outcome::Failed("no reason was recorded".into())));
        record_failure(&paths, &"é".repeat(RECORD_LIMIT));
        let Some(Outcome::Failed(reason)) = take_outcome(&paths, "0.2.0") else { panic!("a long reason is still a failure") };
        assert!(reason.len() <= RECORD_LIMIT + '…'.len_utf8() && reason.ends_with("é…"), "{}", reason.len());
        // A folder where a record goes is not read, and not removed.
        std::fs::create_dir_all(failure_file(&paths)).unwrap();
        std::fs::create_dir_all(done_file(&paths)).unwrap();
        assert_eq!(take_outcome(&paths, "0.2.0"), None);
        assert!(failure_file(&paths).is_dir());
    }

    /// A link where a record goes is not followed: neither record is read through it, and the
    /// file it points to stays. Skipped where the system refuses to create the link (Windows
    /// without the privilege for symbolic links).
    #[test]
    fn a_record_that_is_a_link_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(paths.logs()).unwrap();
        std::fs::create_dir_all(&paths.data).unwrap();
        let elsewhere = dir.path().join("elsewhere.txt");
        std::fs::write(&elsewhere, "v0.2.0").unwrap();
        #[cfg(windows)]
        let link = |to: &Path, at: &Path| std::os::windows::fs::symlink_file(to, at);
        if let Err(e) = link(&elsewhere, &failure_file(&paths)).and_then(|()| link(&elsewhere, &done_file(&paths))) {
            eprintln!("skipped: couldn't create a symbolic link ({e})");
            return;
        }
        assert_eq!(take_failure(&paths), None);
        assert_eq!(take_outcome(&paths, "0.2.0"), None);
        assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "v0.2.0");
    }

    /// Scheduling a new update clears the outcome records of an earlier one, so a stale failure
    /// is not reported after it.
    #[test]
    fn a_new_update_clears_the_last_ones_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        record_failure(&paths, "the Windows Installer stopped with error 1603");
        mark_installed(&paths, &Release { tag: "v0.3.0".into(), ..Release::default() });
        assert_eq!(take_failure(&paths), None);
        assert_eq!(scheduled_version(&paths).as_deref(), Some("v0.3.0"));
    }

    /// A release whose tag is not its version (an app may build its own `Release`) is recorded
    /// by its version, so the new version's first start still reports the update.
    #[test]
    fn a_finished_update_is_recorded_by_its_version_not_its_tag() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let release = Release { tag: "stable".into(), version: version::parse("0.2.0"), ..Release::default() };
        mark_installed(&paths, &release);
        assert!(just_updated_to(&paths, &release), "the tag still guards against offering it again");
        assert_eq!(scheduled_version(&paths).as_deref(), Some("0.2.0"));
        record_done(&paths, &scheduled_version(&paths).unwrap());
        assert_eq!(take_outcome(&paths, "0.2.0"), Some(Outcome::Installed(Version::new(0, 2, 0))));
        // A record without the version line, from before it was written, names the tag.
        crate::paths::write_atomic(&marker(&paths), format!("v0.3.0\n{}\n", crate::now()).as_bytes()).unwrap();
        assert_eq!(scheduled_version(&paths).as_deref(), Some("v0.3.0"));
    }

    /// A launcher that doesn't quit keeps its files, and the download is still removed.
    #[cfg(windows)]
    #[test]
    fn a_launcher_that_keeps_running_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("open3d-launcher.exe");
        std::fs::write(&exe, "0.1.0").unwrap();
        let msi = dir.path().join("open3d-launcher-0.2.0-windows-x64.msi");
        std::fs::write(&msi, "not a package").unwrap();
        let running = std::process::id();
        // The test process itself is still running when the wait asks, so the package is never run.
        let error =
            run_step(&crate::suite::LAUNCHER, &system_program("cmd.exe"), running, &exe, &Finish::Msi(msi.clone()), std::time::Duration::ZERO)
                .unwrap_err();
        assert!(error.contains("still running"), "{error}");
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "0.1.0");
        assert!(!msi.exists());
    }

    #[cfg(windows)]
    fn system_program(name: &str) -> PathBuf {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join(name)
    }

    /// The process id of a process that has already exited.
    #[cfg(windows)]
    fn exited_pid() -> u32 {
        let mut gone = std::process::Command::new("cmd").args(["/C", "exit"]).spawn().unwrap();
        gone.wait().unwrap();
        gone.id()
    }

    /// A signed copy only takes files signed by its own publisher; an unsigned copy (Open3D's
    /// builds, and `cargo test` binaries) compares nothing, as the checksum vouches for the file.
    #[cfg(windows)]
    #[test]
    fn a_signed_copy_takes_only_its_own_publishers_files() {
        let cmd = system_program("cmd.exe");
        let unsigned = std::env::current_exe().unwrap();
        assert_eq!(signed_like(&crate::suite::LAUNCHER, &cmd, &cmd), Ok(()));
        assert!(signed_like(&crate::suite::LAUNCHER, &cmd, &unsigned).is_err_and(|e| e.contains("isn't signed")));
        assert_eq!(signed_like(&crate::suite::LAUNCHER, &unsigned, &cmd), Ok(()));
        assert_eq!(signed_like(&crate::suite::LAUNCHER, &unsigned, &unsigned), Ok(()));
    }

    /// An unsigned launcher still refuses, before it quits, an update of a program that isn't
    /// its own.
    #[cfg(windows)]
    #[test]
    fn an_unsigned_launcher_still_checks_what_it_updates_before_it_quits() {
        let dir = tempfile::tempdir().unwrap();
        let msi = dir.path().join("open3d-launcher-0.2.0-windows-x64.msi");
        std::fs::write(&msi, "not a package").unwrap();
        for install in [Install::Msi(system_program("msiexec.exe")), Install::Portable(system_program("msiexec.exe"))] {
            let error = windows_precheck(&crate::suite::LAUNCHER, &install, &Paths::under(dir.path()), &msi).unwrap_err();
            assert!(!error.contains("isn't signed"), "{error}");
        }
    }

    /// An MSI that isn't signed by the launcher's publisher is never handed to the Windows Installer.
    #[cfg(windows)]
    #[test]
    fn an_unsigned_msi_is_never_run() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("open3d-launcher.exe");
        std::fs::write(&exe, "0.1.0").unwrap();
        let msi = dir.path().join("open3d-launcher-0.2.0-windows-x64.msi");
        std::fs::write(&msi, "not a package").unwrap();
        let error = run_step(
            &crate::suite::LAUNCHER,
            &system_program("cmd.exe"),
            exited_pid(),
            &exe,
            &Finish::Msi(msi.clone()),
            std::time::Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(error.contains("isn't signed"), "{error}");
        assert!(!msi.exists());
    }

    /// A program in the portable zip that isn't signed by the launcher's publisher stops the
    /// update before any file is replaced.
    #[cfg(windows)]
    #[test]
    fn an_unsigned_program_in_the_portable_zip_is_not_installed() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Open3D Launcher");
        std::fs::create_dir_all(&folder).unwrap();
        let exe = folder.join("open3d-launcher.exe");
        std::fs::write(&exe, "0.1.0").unwrap();
        let zip = dir.path().join("open3d-launcher-0.2.0-windows-x64-portable.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&zip).unwrap());
        writer.start_file("open3d-launcher-0.2.0-windows-x64-portable/README.md", zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(b"# Open3D Launcher").unwrap();
        writer.start_file("open3d-launcher-0.2.0-windows-x64-portable/open3d-launcher.exe", zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(&std::fs::read(system_program("cmd.exe")).unwrap()).unwrap();
        // An unsigned program under a harmless name.
        writer.start_file("open3d-launcher-0.2.0-windows-x64-portable/notes.txt", zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(&std::fs::read(std::env::current_exe().unwrap()).unwrap()).unwrap();
        writer.finish().unwrap();
        let tmp = dir.path().join("cache").join(PORTABLE_STAGING);
        let step = Finish::Portable { zip: zip.clone(), tmp: tmp.clone() };
        let error =
            run_step(&crate::suite::LAUNCHER, &system_program("cmd.exe"), exited_pid(), &exe, &step, std::time::Duration::from_secs(5)).unwrap_err();
        assert!(error.contains("isn't signed"), "{error}");
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "0.1.0");
        assert!(!folder.join("README.md").exists() && !folder.join("notes.txt").exists(), "nothing is copied");
        assert!(!zip.exists() && !tmp.exists());
    }

    /// A missing or malformed executable must be rejected before any installed file is changed.
    #[cfg(windows)]
    #[test]
    fn a_portable_update_requires_a_signed_launcher_even_without_an_mz_header() {
        use std::io::Write;
        for launcher in [None, Some(b"not a Windows program".as_slice()), Some(b"".as_slice())] {
            let dir = tempfile::tempdir().unwrap();
            let folder = dir.path().join("Open3D Launcher");
            std::fs::create_dir_all(folder.join("Open3DLauncherData")).unwrap();
            let exe = folder.join("open3d-launcher.exe");
            std::fs::write(&exe, "old launcher").unwrap();
            let settings = folder.join("Open3DLauncherData/settings.json");
            std::fs::write(&settings, "settings").unwrap();
            let zip = dir.path().join("update.zip");
            let mut writer = zip::ZipWriter::new(std::fs::File::create(&zip).unwrap());
            writer.start_file("launcher/README.md", zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(b"new readme").unwrap();
            if let Some(body) = launcher {
                writer.start_file("launcher/open3d-launcher.exe", zip::write::SimpleFileOptions::default()).unwrap();
                writer.write_all(body).unwrap();
            }
            writer.finish().unwrap();
            let tmp = dir.path().join(PORTABLE_STAGING);
            let step = Finish::Portable { zip: zip.clone(), tmp: tmp.clone() };
            let error = run_step(&crate::suite::LAUNCHER, &system_program("cmd.exe"), exited_pid(), &exe, &step, std::time::Duration::from_secs(5))
                .unwrap_err();
            assert!(error.contains(if launcher.is_some() { "isn't signed" } else { "doesn't contain" }), "{error}");
            assert_eq!(std::fs::read(&exe).unwrap(), b"old launcher");
            assert_eq!(std::fs::read(&settings).unwrap(), b"settings");
            assert!(!folder.join("README.md").exists());
            assert!(!zip.exists() && !tmp.exists());
        }
    }

    /// A copy that fails part way through puts the previous files back and removes the new ones,
    /// so the launcher that starts again is the old one, whole.
    #[cfg(windows)]
    #[test]
    fn a_portable_update_that_fails_part_way_puts_the_old_files_back() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Open3D Launcher");
        // A folder where the zip has a file: copying it there fails after the exe was replaced.
        std::fs::create_dir_all(folder.join("NOTICE")).unwrap();
        let exe = folder.join("open3d-launcher.exe");
        std::fs::write(&exe, "0.1.0").unwrap();
        let zip = dir.path().join("open3d-launcher-0.2.0-windows-x64-portable.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&zip).unwrap());
        let signed_exe = std::fs::read(system_program("cmd.exe")).unwrap();
        for (name, body) in [
            ("open3d-launcher-0.2.0-windows-x64-portable/open3d-launcher.exe", signed_exe.as_slice()),
            ("open3d-launcher-0.2.0-windows-x64-portable/README.md", b"# Open3D Launcher".as_slice()),
            ("open3d-launcher-0.2.0-windows-x64-portable/NOTICE", b"notice".as_slice()),
        ] {
            writer.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap();
        let tmp = dir.path().join("cache").join(PORTABLE_STAGING);
        let step = Finish::Portable { zip: zip.clone(), tmp: tmp.clone() };
        let error =
            run_step(&crate::suite::LAUNCHER, &system_program("cmd.exe"), exited_pid(), &exe, &step, std::time::Duration::from_secs(5)).unwrap_err();
        assert!(error.contains("NOTICE"), "{error}");
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "0.1.0", "the previous exe is put back");
        assert!(!folder.join("README.md").exists(), "a file the update added is removed");
        assert!(!zip.exists() && !tmp.exists());
    }

    #[test]
    fn a_corrupt_self_update_timestamp_does_not_overflow() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let release = Release { tag: "v0.2.0".into(), ..Release::default() };
        crate::paths::write_atomic(&marker(&paths), format!("v0.2.0\n{}\n", i64::MIN).as_bytes()).unwrap();
        assert!(!just_updated_to(&paths, &release));
    }

    #[test]
    fn a_future_self_update_timestamp_does_not_suppress_updates() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let release = Release { tag: "v0.2.0".into(), ..Release::default() };
        for at in [crate::now() + JUST_UPDATED_FOR, i64::MAX] {
            crate::paths::write_atomic(&marker(&paths), format!("v0.2.0\n{at}\n").as_bytes()).unwrap();
            assert!(!just_updated_to(&paths, &release), "future timestamp {at}");
        }
    }

    #[test]
    fn a_version_just_installed_is_not_offered_again() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let r = Release { tag: "v0.2.0".into(), ..Release::default() };
        assert!(!just_updated_to(&paths, &r));
        crate::paths::write_atomic(&marker(&paths), format!("v0.2.0\n{}\n", crate::now()).as_bytes()).unwrap();
        assert!(just_updated_to(&paths, &r));
        assert!(!just_updated_to(&paths, &Release { tag: "v0.3.0".into(), ..Release::default() }));
        // A day later the guard lapses.
        crate::paths::write_atomic(&marker(&paths), format!("v0.2.0\n{}\n", crate::now() - JUST_UPDATED_FOR - 1).as_bytes()).unwrap();
        assert!(!just_updated_to(&paths, &r));
    }

    #[test]
    fn restarted_apps_do_not_inherit_their_pretend_versions() {
        for app in crate::suite::SUITE.iter().chain(std::iter::once(&crate::suite::LAUNCHER)) {
            let mut cmd = std::process::Command::new("unused");
            clear_pretend_version(&mut cmd, app);
            for key in [PRETEND_VAR.to_string(), pretend_var(app)] {
                assert!(cmd.get_envs().any(|(k, v)| k == key.as_str() && v.is_none()), "{} retained {key}", app.id);
            }
        }
    }

    /// A directory laid out like Cargo's target tree: `.rustc_info.json` at the root and the
    /// executable below `profile` (with or without a target triple in between).
    fn cargo_build_tree(dir: &Path, root: &str, relative_exe: &str) -> PathBuf {
        let root = dir.join(root);
        let exe = root.join(relative_exe);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(root.join(".rustc_info.json"), "{}").unwrap();
        std::fs::write(&exe, "ELF").unwrap();
        exe
    }

    /// A directory laid out like a Cargo target tree whose rustc-info cache was disabled: the
    /// target root has `CACHEDIR.TAG`, but no `.rustc_info.json`. `profile` is relative to the
    /// target root, so it may include a target triple.
    fn cargo_build_tree_without_rustc_info(dir: &Path, profile: &str, exe: &str) -> PathBuf {
        let root = dir.join("target");
        let profile = root.join(profile);
        for sub in [".fingerprint", "deps", "build"] {
            std::fs::create_dir_all(profile.join(sub)).unwrap();
        }
        let exe = root.join(exe);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(root.join(CARGO_CACHE_MARKER), "Signature: 8a477f597d28d172789f06886806bc55\n").unwrap();
        std::fs::write(&exe, "ELF").unwrap();
        exe
    }

    /// `CARGO_TARGET_DIR` moves the build tree; the executable in it is still a development build,
    /// whatever the profile or target triple is called.
    #[test]
    fn a_development_build_under_a_custom_target_dir_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        for exe in [
            "debug/deps/craft_update-e8899812898c870d",
            "release/open3d-launcher",
            "x86_64-pc-windows-gnu/debug/deps/craft_update-0123456789abcdef",
            "x86_64-pc-windows-gnu/my-profile/deps/open3d-launcher-0123456789abcdef",
        ] {
            let exe = cargo_build_tree(dir.path(), "builds/egui037-craft-libs", exe);
            assert!(is_development_build(&exe, false), "{}", exe.display());
        }
    }

    /// A Cargo build tree is not recognised by names: an installed copy in a folder literally
    /// called `target` stays an installed copy.
    #[test]
    fn an_installed_copy_in_a_folder_called_target_is_not_a_development_build() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("target").join("Open3D Launcher");
        std::fs::create_dir_all(&folder).unwrap();
        let exe = folder.join("open3d-launcher.exe");
        std::fs::write(&exe, "MZ").unwrap();
        assert!(!is_development_build(&exe, false), "a folder named target is not a build tree");
    }

    /// `CACHEDIR.TAG` has a generic signature, used by every cache directory; without Cargo's
    /// profile layout it does not identify a development build.
    #[test]
    fn a_generic_cache_marker_is_not_a_cargo_target_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let exe = cache.join("Open3D Launcher").join("open3d-launcher");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(cache.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172789f06886806bc55\n").unwrap();
        std::fs::write(&exe, "ELF").unwrap();
        assert!(!is_development_build(&exe, false), "a generic cache is not a Cargo build tree");
    }

    /// `CARGO_CACHE_RUSTC_INFO=0` leaves no `.rustc_info.json`; the profile layout plus the
    /// target root's `CACHEDIR.TAG` still identify a development build, whatever the profile or
    /// target triple is called.
    #[test]
    fn a_cargo_build_tree_without_rustc_info_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        for (profile, exe) in [
            ("debug", "debug/deps/craft_update-e8899812898c870d"),
            ("release", "release/open3d-launcher"),
            ("my-profile", "my-profile/open3d-launcher"),
            ("x86_64-pc-windows-gnu/my-profile", "x86_64-pc-windows-gnu/my-profile/deps/open3d-launcher-0123456789abcdef"),
        ] {
            let exe = cargo_build_tree_without_rustc_info(dir.path(), profile, exe);
            assert!(is_development_build(&exe, false), "{}", exe.display());
        }
    }

    /// The fallback needs the complete profile layout and the target root's cache marker; a
    /// partial layout or a missing marker isn't enough.
    #[test]
    fn an_incomplete_cargo_layout_is_not_a_development_build() {
        let dir = tempfile::tempdir().unwrap();
        for missing in [".fingerprint", "deps", "build"] {
            let exe = cargo_build_tree_without_rustc_info(dir.path(), "debug", "debug/deps/open3d-launcher");
            std::fs::remove_dir_all(dir.path().join("target/debug").join(missing)).unwrap();
            assert!(!is_development_build(&exe, false), "missing {missing}");
        }

        let dir = tempfile::tempdir().unwrap();
        let exe = cargo_build_tree_without_rustc_info(dir.path(), "debug", "debug/deps/open3d-launcher");
        std::fs::remove_file(dir.path().join("target").join(CARGO_CACHE_MARKER)).unwrap();
        assert!(!is_development_build(&exe, false), "a profile layout without the target-root marker");
    }

    /// An installed copy that merely lives below a Cargo project isn't in that project's target
    /// tree, even when the project has a build tree of its own.
    #[test]
    fn an_installed_copy_beneath_a_cargo_project_is_not_a_development_build() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(project.join("Cargo.toml"), "[package]\nname = \"installer\"\n").unwrap();
        cargo_build_tree_without_rustc_info(&project, "debug", "debug/deps/installer-0123456789abcdef");
        let exe = project.join("dist").join("open3d-launcher");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "ELF").unwrap();
        assert!(!is_development_build(&exe, false), "{}", exe.display());
    }

    /// Both markers must be files: a directory of the same name doesn't identify or corroborate
    /// a build tree.
    #[test]
    fn marker_directories_are_not_development_builds() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("builds").join("debug").join("deps").join("open3d-launcher");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::create_dir_all(dir.path().join("builds").join(".rustc_info.json")).unwrap();
        std::fs::write(&exe, "ELF").unwrap();
        assert!(!is_development_build(&exe, false), "a directory named .rustc_info.json");

        let dir = tempfile::tempdir().unwrap();
        let exe = cargo_build_tree_without_rustc_info(dir.path(), "debug", "debug/deps/open3d-launcher");
        let root = dir.path().join("target");
        std::fs::remove_file(root.join(CARGO_CACHE_MARKER)).unwrap();
        std::fs::create_dir(root.join(CARGO_CACHE_MARKER)).unwrap();
        assert!(!is_development_build(&exe, false), "a directory named CACHEDIR.TAG");
    }

    /// `CRAFT_UPDATE_DEV` still lets a development build try the self-update.
    #[test]
    fn a_development_build_can_opt_in_to_updating_itself() {
        let dir = tempfile::tempdir().unwrap();
        let exe = cargo_build_tree(dir.path(), "builds/egui037-craft-libs", "release/open3d-launcher");
        assert!(!is_development_build(&exe, true));
    }

    /// The opt-in also covers a build only recognised through the cache-disabled fallback.
    #[test]
    fn a_development_build_without_rustc_info_can_opt_in_to_updating_itself() {
        let dir = tempfile::tempdir().unwrap();
        let exe = cargo_build_tree_without_rustc_info(dir.path(), "release", "release/open3d-launcher");
        assert!(!is_development_build(&exe, true));
    }

    #[test]
    fn a_development_build_does_not_update_itself() {
        // An archived test runner may relocate its executable without Cargo's metadata.
        // Supply a complete cache-disabled build tree so this checks the updater guard,
        // independent of the runner's location and the ambient `CRAFT_UPDATE_DEV` value.
        let dir = tempfile::tempdir().unwrap();
        let exe = cargo_build_tree_without_rustc_info(dir.path(), "debug", "debug/deps/open3d-launcher");
        assert!(is_development_build(&exe, false), "{} should be inside a Cargo target directory", exe.display());
        assert_eq!(current_at(&crate::suite::LAUNCHER, &exe, false), None);
    }

    /// The guard in `current_at` runs before platform detection, so a development executable is
    /// rejected without probing the ambient host.
    #[test]
    fn current_at_rejects_a_development_build_before_detection() {
        let dir = tempfile::tempdir().unwrap();
        let exe = cargo_build_tree(dir.path(), "builds/egui037-craft-libs", "debug/open3d-launcher");
        assert_eq!(current_at(&crate::suite::LAUNCHER, &exe, false), None);
    }

    #[test]
    fn the_launcher_picks_its_own_release_files() {
        let names = [
            "open3d-launcher-0.2.0-macos-universal.dmg",
            "open3d-launcher-0.2.0-windows-x64.msi",
            "open3d-launcher-0.2.0-windows-x64-portable.zip",
            "open3d-launcher-0.2.0-linux-x86_64.AppImage",
            "open3d-launcher-0.2.0-linux-x86_64.deb",
            "open3d-launcher-0.2.0-linux-x86_64.flatpak",
            "open3d-launcher-0.2.0-linux-x86_64.pkg.tar.zst",
            "open3d-launcher-web-0.2.0.zip",
        ];
        let release = Release {
            assets: names.iter().map(|n| crate::release::Asset { name: (*n).into(), size: 1, url: String::new(), sha256: None }).collect(),
            ..Release::default()
        };
        use crate::{Arch, Os};
        let pick =
            |os, kind| pick_asset(&release, &[crate::suite::LAUNCHER.id], Platform { os, arch: Arch::X64 }, &[kind]).map(|(a, _)| a.name.clone());
        assert_eq!(pick(Os::MacOs, Install::MacApp(PathBuf::new()).kind()).as_deref(), Some(names[0]));
        assert_eq!(pick(Os::Windows, Install::Msi(PathBuf::new()).kind()).as_deref(), Some(names[1]));
        assert_eq!(pick(Os::Windows, Install::Portable(PathBuf::new()).kind()).as_deref(), Some(names[2]));
        assert_eq!(pick(Os::Linux, Install::AppImage(PathBuf::new()).kind()).as_deref(), Some(names[3]));
        assert_eq!(pick(Os::Linux, Install::Package(PackageKind::Deb, PathBuf::new()).kind()).as_deref(), Some(names[4]));
        assert_eq!(pick(Os::Linux, Install::Flatpak.kind()).as_deref(), Some(names[5]));
        assert_eq!(pick(Os::Linux, Install::Package(PackageKind::ArchPkg, PathBuf::new()).kind()).as_deref(), Some(names[6]));
    }

    #[test]
    fn a_failed_step_that_removed_the_app_starts_nothing_and_keeps_its_message() {
        assert!(restarts(true, true, true));
        assert!(restarts(true, true, false));
        assert!(restarts(true, false, true));
        assert!(!restarts(true, false, false));
    }

    /// An app still running after the helper's wait never gets a second copy, whatever the step
    /// and the exe would allow.
    #[test]
    fn an_app_still_running_is_never_started_again() {
        for (step_succeeded, exe_exists) in [(true, true), (true, false), (false, true), (false, false)] {
            assert!(!restarts(false, step_succeeded, exe_exists), "{step_succeeded} {exe_exists}");
        }
    }

    /// A helper whose wait ends with the old app still running installs nothing, starts no
    /// program, records why, and removes the download.
    #[cfg(windows)]
    #[test]
    fn the_helper_starts_nothing_while_the_old_app_runs() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let exe = dir.path().join("open3d-launcher.exe");
        std::fs::write(&exe, "0.1.0").unwrap();
        let zip = dir.path().join("open3d-launcher-0.2.0-windows-x64-portable.zip");
        std::fs::write(&zip, "zip").unwrap();
        let step = Finish::Portable { zip: zip.clone(), tmp: dir.path().join(PORTABLE_STAGING) };
        let started = std::sync::Mutex::new(Vec::<PathBuf>::new());
        let start = |exe: &Path| {
            started.lock().unwrap().push(exe.to_path_buf());
            Ok(())
        };
        let never_exits = |_pid| false;
        let error = finish_step(&crate::suite::LAUNCHER, &system_program("cmd.exe"), &paths, 4242, exe.clone(), &step, false, &never_exits, &start)
            .unwrap_err();
        assert!(error.contains("still running"), "{error}");
        assert_eq!(*started.lock().unwrap(), Vec::<PathBuf>::new(), "no copy of the app may start while it still runs");
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "0.1.0");
        assert!(!zip.exists(), "the download is removed");
        assert_eq!(take_failure(&paths).as_deref(), Some(error.as_str()));
    }

    /// A step that fails after the old app exited starts the old app again, as it was.
    #[cfg(windows)]
    #[test]
    fn the_helper_starts_the_old_app_after_a_failed_step() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let exe = dir.path().join("open3d-launcher.exe");
        std::fs::write(&exe, "0.1.0").unwrap();
        // Not a zip, so the step fails.
        let zip = dir.path().join("open3d-launcher-0.2.0-windows-x64-portable.zip");
        std::fs::write(&zip, "not a zip").unwrap();
        let step = Finish::Portable { zip, tmp: dir.path().join(PORTABLE_STAGING) };
        let started = std::sync::Mutex::new(Vec::<PathBuf>::new());
        let start = |exe: &Path| {
            started.lock().unwrap().push(exe.to_path_buf());
            Ok(())
        };
        let exited = |_pid| true;
        assert!(finish_step(&crate::suite::LAUNCHER, &system_program("cmd.exe"), &paths, 4242, exe.clone(), &step, false, &exited, &start).is_err());
        assert_eq!(*started.lock().unwrap(), vec![exe.clone()]);
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "0.1.0");
    }

    /// When the update step fails and the old app can't be started again either, the error kept
    /// for the next start names both.
    #[cfg(windows)]
    #[test]
    fn a_failed_step_and_a_failed_restart_are_both_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let exe = dir.path().join("open3d-launcher.exe");
        std::fs::write(&exe, "0.1.0").unwrap();
        // Not a zip, so the step fails.
        let zip = dir.path().join("open3d-launcher-0.2.0-windows-x64-portable.zip");
        std::fs::write(&zip, "not a zip").unwrap();
        let step = Finish::Portable { zip, tmp: dir.path().join(PORTABLE_STAGING) };
        let start = |_exe: &Path| Err("couldn't start open3d-launcher.exe (denied)".to_string());
        let exited = |_pid| true;
        let error =
            finish_step(&crate::suite::LAUNCHER, &system_program("cmd.exe"), &paths, 4242, exe.clone(), &step, false, &exited, &start).unwrap_err();
        assert!(error.ends_with("; couldn't start open3d-launcher.exe (denied)"), "{error}");
        assert!(error.len() > "; couldn't start open3d-launcher.exe (denied)".len(), "the step's error comes first: {error}");
        assert_eq!(take_failure(&paths).as_deref(), Some(error.as_str()));
    }

    /// The helper's result keeps the update step's error when the start fails too.
    #[test]
    fn a_failed_restart_keeps_the_step_error() {
        assert_eq!(with_restart(Ok(()), Ok(())), Ok(()));
        assert_eq!(with_restart(Err("step".into()), Ok(())), Err("step".into()));
        assert_eq!(with_restart(Ok(()), Err("start".into())), Err("start".into()));
        assert_eq!(with_restart(Err("step".into()), Err("start".into())), Err("step; start".into()));
    }

    /// A successful step starts the new app once.
    #[cfg(windows)]
    #[test]
    fn the_helper_starts_the_new_app_after_the_update() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let folder = dir.path().join("Open3D Launcher");
        std::fs::create_dir_all(&folder).unwrap();
        let exe = folder.join("open3d-launcher.exe");
        std::fs::write(&exe, "0.1.0").unwrap();
        let zip = dir.path().join("open3d-launcher-0.2.0-windows-x64-portable.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&zip).unwrap());
        let signed_exe = std::fs::read(system_program("cmd.exe")).unwrap();
        writer.start_file("open3d-launcher-0.2.0-windows-x64-portable/open3d-launcher.exe", zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(&signed_exe).unwrap();
        writer.finish().unwrap();
        let step = Finish::Portable { zip, tmp: dir.path().join(PORTABLE_STAGING) };
        let started = std::sync::Mutex::new(Vec::<(PathBuf, Vec<u8>)>::new());
        let start = |exe: &Path| {
            started.lock().unwrap().push((exe.to_path_buf(), std::fs::read(exe).unwrap()));
            Ok(())
        };
        let exited = |_pid| true;
        mark_installed(&paths, &Release { tag: "v0.2.0".into(), ..Release::default() });
        assert_eq!(
            finish_step(&crate::suite::LAUNCHER, &system_program("cmd.exe"), &paths, 4242, exe.clone(), &step, false, &exited, &start),
            Ok(())
        );
        assert_eq!(*started.lock().unwrap(), vec![(exe, signed_exe)], "the new app starts once");
        assert_eq!(take_outcome(&paths, "0.2.0"), Some(Outcome::Installed(Version::new(0, 2, 0))), "the new version's first start says so");
    }

    /// A standalone copy updated from the portable zip becomes portable: the outcome and the
    /// record of the update go to the folders beside it, which the new app reads.
    #[cfg(windows)]
    #[test]
    fn a_standalone_copy_that_becomes_portable_finds_the_outcome_beside_it() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        // The per-user folders the old, standalone copy used.
        let paths = Paths::under(&dir.path().join("per-user"));
        let folder = dir.path().join("Open3D Launcher");
        std::fs::create_dir_all(&folder).unwrap();
        let exe = folder.join("open3d-launcher.exe");
        std::fs::write(&exe, "0.1.0").unwrap();
        let zip = dir.path().join("open3d-launcher-0.2.0-windows-x64-portable.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&zip).unwrap());
        writer.start_file("open3d-launcher-0.2.0-windows-x64-portable/open3d-launcher.exe", zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(&std::fs::read(system_program("cmd.exe")).unwrap()).unwrap();
        writer.start_file("open3d-launcher-0.2.0-windows-x64-portable/portable.txt", zip::write::SimpleFileOptions::default()).unwrap();
        writer.finish().unwrap();
        let step = Finish::Portable { zip, tmp: dir.path().join(PORTABLE_STAGING) };
        let release = Release { tag: "v0.2.0".into(), version: version::parse("0.2.0"), ..Release::default() };
        mark_installed(&paths, &release);
        let exited = |_pid| true;
        let start = |_: &Path| Ok(());
        assert_eq!(
            finish_step(&crate::suite::LAUNCHER, &system_program("cmd.exe"), &paths, 4242, exe.clone(), &step, false, &exited, &start),
            Ok(())
        );
        let after = Paths::for_exe(&crate::suite::LAUNCHER, &exe).unwrap();
        assert_eq!(after.data, folder.join("Open3DLauncherData").join("data"));
        assert!(just_updated_to(&after, &release), "the new app does not offer the same update again");
        assert_eq!(take_outcome(&after, "0.2.0"), Some(Outcome::Installed(Version::new(0, 2, 0))));
        assert_eq!(take_outcome(&paths, "0.2.0"), None, "nothing is left where the new app no longer looks");

        // A new app that cannot start is reported there too.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(&dir.path().join("per-user"));
        let (exe, step) = portable_update(dir.path());
        if let Finish::Portable { zip, .. } = &step {
            let mut writer = zip::ZipWriter::new(std::fs::File::create(zip).unwrap());
            writer.start_file("open3d-launcher-0.2.0-windows-x64-portable/open3d-launcher.exe", zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(&std::fs::read(system_program("cmd.exe")).unwrap()).unwrap();
            writer.start_file("open3d-launcher-0.2.0-windows-x64-portable/portable.txt", zip::write::SimpleFileOptions::default()).unwrap();
            writer.finish().unwrap();
        }
        mark_installed(&paths, &release);
        let denied = |_: &Path| Err("couldn't start the launcher (denied)".to_owned());
        assert!(finish_step(&crate::suite::LAUNCHER, &system_program("cmd.exe"), &paths, 4242, exe.clone(), &step, false, &exited, &denied).is_err());
        let after = Paths::for_exe(&crate::suite::LAUNCHER, &exe).unwrap();
        assert_eq!(take_outcome(&after, "0.2.0"), Some(Outcome::Failed("couldn't start the launcher (denied)".into())));
    }

    /// A portable update of the launcher to 0.2.0 in `dir`: its exe and the step.
    #[cfg(windows)]
    fn portable_update(dir: &Path) -> (PathBuf, Finish) {
        use std::io::Write;
        let folder = dir.join("Open3D Launcher");
        std::fs::create_dir_all(&folder).unwrap();
        let exe = folder.join("open3d-launcher.exe");
        std::fs::write(&exe, "0.1.0").unwrap();
        let zip = dir.join("open3d-launcher-0.2.0-windows-x64-portable.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&zip).unwrap());
        writer.start_file("open3d-launcher-0.2.0-windows-x64-portable/open3d-launcher.exe", zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(&std::fs::read(system_program("cmd.exe")).unwrap()).unwrap();
        writer.finish().unwrap();
        (exe, Finish::Portable { zip, tmp: dir.join(PORTABLE_STAGING) })
    }

    /// A finished step whose new app can't be started is a failure at the next start, never a
    /// finished update.
    #[cfg(windows)]
    #[test]
    fn a_failed_relaunch_is_not_reported_as_a_finished_update() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let (exe, step) = portable_update(dir.path());
        mark_installed(&paths, &Release { tag: "v0.2.0".into(), ..Release::default() });
        let start = |_: &Path| Err("couldn't start the launcher (denied)".to_owned());
        let exited = |_pid| true;
        let result = finish_step(&crate::suite::LAUNCHER, &system_program("cmd.exe"), &paths, 4242, exe, &step, false, &exited, &start);
        assert_eq!(result, Err("couldn't start the launcher (denied)".into()));
        assert_eq!(take_outcome(&paths, "0.2.0"), Some(Outcome::Failed("couldn't start the launcher (denied)".into())));
    }

    /// A helper that times out or whose step fails records no finished update.
    #[cfg(windows)]
    #[test]
    fn a_timed_out_or_failed_step_is_not_reported_as_a_finished_update() {
        for (exits, zip_ok) in [(false, true), (true, false)] {
            let dir = tempfile::tempdir().unwrap();
            let paths = Paths::under(dir.path());
            let (exe, step) = portable_update(dir.path());
            if !zip_ok && let Finish::Portable { zip, .. } = &step {
                std::fs::write(zip, "not a zip").unwrap();
            }
            mark_installed(&paths, &Release { tag: "v0.2.0".into(), ..Release::default() });
            let start = |_: &Path| Ok(());
            let wait = |_pid| exits;
            assert!(finish_step(&crate::suite::LAUNCHER, &system_program("cmd.exe"), &paths, 4242, exe, &step, false, &wait, &start).is_err());
            assert!(!done_file(&paths).exists(), "exits {exits}, zip {zip_ok}");
            assert!(matches!(take_outcome(&paths, "0.2.0"), Some(Outcome::Failed(_))), "exits {exits}, zip {zip_ok}");
        }
    }

    #[test]
    fn update_refuses_to_downgrade_when_release_is_older() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let http = Http::new(crate::TEST_AGENT).unwrap();
        let mut phase = |_| ();
        let last = || Ok(());
        let stop = Stop::default();
        let release = Release {
            version: version::parse("0.1.0"),
            tag: "v0.1.0".into(),
            assets: vec![crate::release::Asset {
                name: "open3d-launcher-0.1.0-linux-x86_64.AppImage".into(),
                size: 1,
                url: String::new(),
                sha256: None,
            }],
            ..Release::default()
        };
        let install = Install::AppImage(dir.path().join("launcher.AppImage"));
        use crate::{Arch, Os};
        let platform = Platform { os: Os::Linux, arch: Arch::X64 };
        let result = update(&crate::suite::LAUNCHER, &install, &release, platform, &http, &paths, &stop, &mut phase, &last, Some("0.2.0"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("is not newer than currently running version"));
    }

    #[test]
    fn a_build_from_main_refuses_to_install_the_release_it_already_contains() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let http = Http::new(crate::TEST_AGENT).unwrap();
        let mut phase = |_| ();
        let last = || Ok(());
        let stop = Stop::default();
        let release = Release { version: version::parse("0.2.0"), tag: "v0.2.0".into(), ..Release::default() };
        let install = Install::AppImage(dir.path().join("launcher.AppImage"));
        use crate::{Arch, Os};
        let platform = Platform { os: Os::Linux, arch: Arch::X64 };
        let running = Some("0.2.0-integration.20261029.f5b3f123");
        let result = update(&crate::suite::LAUNCHER, &install, &release, platform, &http, &paths, &stop, &mut phase, &last, running);
        assert!(result.unwrap_err().contains("is not newer than currently running version"));
    }
}
