// SPDX-License-Identifier: GPL-3.0-or-later
//! Open3D apps that are programs of their own: installing, updating, finding and removing them.
//!
//! A release asset (`<app>-<version>-windows-x64.zip`) holds `<App>.exe` and its files at its root,
//! or the program where the catalog's `exe` says (`bin/OpenGaussian.exe`, for a program that needs
//! the folders around it). They go in `%LOCALAPPDATA%\Programs\Open3D\<App>`, beside the launcher's
//! own folder: per user, so no administrator rights are needed. A first install adds `<App>.lnk` to
//! the Start menu and to the desktop.
//!
//! Work happens in `.open3d-<id>` beside the app folders, as for 3ds Max bundles
//! ([`crate::max_bundle`]): the new copy is unpacked and checked in `new\<App>`, the old one waits in
//! `backup\<App>` until the new one is in place, and [`recover`] puts a backup back after an
//! interrupted update. The exe's own file properties say which version a copy is, so a copy put
//! there by hand is found too.
//!
//! Open3D's own code, not part of ArtCraft Launcher.

use std::path::{Path, PathBuf};

use open3d_launcher_core::{InstallSource, Installed, PackageKind, SuiteApp, version};

use crate::manifest::Record;

/// The folders a standalone app touches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Places {
    /// `%LOCALAPPDATA%\Programs\Open3D`: one folder per app.
    pub apps: PathBuf,
    /// Where a first install leaves a shortcut: the Start menu's Programs folder and the desktop.
    pub shortcuts: Vec<PathBuf>,
}

impl Places {
    /// This account's folders.
    pub fn system() -> Option<Self> {
        // Development builds only: installs go under a test folder (the release build ignores it).
        if cfg!(debug_assertions)
            && let Some(root) = std::env::var_os("OPEN3D_LAUNCHER_APPS_ROOT")
        {
            return Some(Self::under(Path::new(&root)));
        }
        Some(Self { apps: dirs::data_local_dir()?.join("Programs").join("Open3D"), shortcuts: shortcut_folders() })
    }

    /// Everything under one folder: tests, and development builds with `OPEN3D_LAUNCHER_APPS_ROOT`.
    pub(crate) fn under(root: &Path) -> Self {
        Self { apps: root.join("Programs").join("Open3D"), shortcuts: vec![root.join("Start Menu"), root.join("Desktop")] }
    }
}

/// The Start menu's Programs folder and the desktop, as Windows names them for this account (a
/// desktop moved to another drive or into a synced folder is followed).
fn shortcut_folders() -> Vec<PathBuf> {
    use winsafe::{SHGetKnownFolderPath, co};
    [co::KNOWNFOLDERID::Programs, co::KNOWNFOLDERID::Desktop]
        .iter()
        .filter_map(|id| SHGetKnownFolderPath(id, co::KF::DEFAULT, None).ok())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .collect()
}

/// The program as the package and messages name it: `<App name>.exe` (`PhotoTool.exe`), or the
/// catalog's path inside the folder (`bin/OpenGaussian.exe`).
pub fn exe_name(app: &SuiteApp) -> String {
    app.exe_relative()
}

/// The program inside a folder that holds the app.
pub fn exe_in(dir: &Path, app: &SuiteApp) -> PathBuf {
    app.exe_relative().split('/').fold(dir.to_path_buf(), |path, part| path.join(part))
}

/// The app's folder in `places`, named after the app.
pub fn app_dir(app: &SuiteApp, places: &Places) -> PathBuf {
    places.apps.join(app.name)
}

/// Where this app's program lives on this account.
pub fn exe_path(app: &SuiteApp) -> Option<PathBuf> {
    Places::system().map(|places| exe_in(&app_dir(app, &places), app))
}

/// Reads the version a program says it is. Tests pass their own reader.
pub(crate) type VersionOf<'a> = &'a dyn Fn(&Path) -> Option<String>;

/// The version in a program's file properties: its product version when that reads as a version
/// (`0.2.0-beta.1`), else the first three numbers of its file version. `None` for a file without
/// version information.
pub fn exe_version(exe: &Path) -> Option<String> {
    let info = winsafe::HVERSIONINFO::GetFileVersionInfo(exe.to_str()?).ok()?;
    let named = info
        .langs_and_cps()
        .ok()
        .and_then(|pairs| pairs.iter().find_map(|pair| info.str_val(*pair, "ProductVersion").ok()))
        .map(|text| text.trim().to_string())
        .filter(|text| version::parse(text).is_some());
    named.or_else(|| {
        let [major, minor, patch, _] = info.version_info().ok()?.dwFileVersion();
        Some(format!("{major}.{minor}.{patch}"))
    })
}

/// The installed program, if there is one. A copy the launcher didn't install (unzipped by hand)
/// is found too: it can be updated and removed the same way.
pub fn detect(app: &SuiteApp) -> Option<Installed> {
    detect_in(app, &Places::system()?, &exe_version)
}

/// [`detect`] in `places`.
pub(crate) fn detect_in(app: &SuiteApp, places: &Places, version_of: VersionOf<'_>) -> Option<Installed> {
    let exe = exe_in(&app_dir(app, places), app);
    if !exe.is_file() {
        return None;
    }
    Some(Installed {
        version: version_of(&exe).as_deref().and_then(version::parse),
        source: InstallSource::AppFolder,
        location: exe.to_string_lossy().into_owned(),
        package: None,
        externally_managed: false,
        origin: None,
        installation: None,
    })
}

/// Whether the program at `exe` is running. Windows lets nobody open a running program's file for
/// writing, so this asks about that very file: another copy of the app started from somewhere
/// else doesn't count.
pub fn running(exe: &Path) -> bool {
    // ERROR_SHARING_VIOLATION, ERROR_USER_MAPPED_FILE.
    const IN_USE: [i32; 2] = [32, 1224];
    match std::fs::OpenOptions::new().write(true).open(exe) {
        Ok(_) => false,
        Err(e) => e.raw_os_error().is_some_and(|code| IN_USE.contains(&code)),
    }
}

/// The error for anything that must wait until the app is closed.
pub fn running_error(app: &SuiteApp) -> String {
    format!("{} is running. Quit it, then try again.", app.name)
}

fn work_dir(apps: &Path, app: &SuiteApp) -> PathBuf {
    apps.join(format!(".open3d-{}", app.id))
}

/// Puts back an old copy left aside by an interrupted update, then removes the work folder.
pub fn recover(app: &SuiteApp, apps: &Path) -> Result<(), String> {
    let work = work_dir(apps, app);
    if !work.exists() {
        return Ok(());
    }
    let target = apps.join(app.name);
    let backup = work.join("backup").join(app.name);
    if backup.is_dir() && !target.exists() {
        std::fs::rename(&backup, &target).map_err(|e| format!("Couldn't restore {} ({e})", target.display()))?;
    }
    std::fs::remove_dir_all(&work).map_err(|e| format!("Couldn't clean {} ({e})", work.display()))
}

/// Installs or updates `app` from the downloaded `zip`; the program in it must say it is `version`.
pub fn install(app: &SuiteApp, zip: &Path, version: &str) -> Result<Record, String> {
    install_into(app, zip, version, &Places::system().ok_or("no per-user programs folder on this system")?, &exe_version)
}

/// [`install`] into `places`.
pub(crate) fn install_into(app: &SuiteApp, zip: &Path, version: &str, places: &Places, version_of: VersionOf<'_>) -> Result<Record, String> {
    if !open3d_launcher_core::hosted_catalog::windows_file_name(app.name) {
        return Err(format!("{} can't be a folder's name, so it wasn't installed", app.name));
    }
    let apps = &places.apps;
    std::fs::create_dir_all(apps).map_err(|e| format!("Couldn't create {} ({e})", apps.display()))?;
    let target = apps.join(app.name);
    let exe = exe_in(&target, app);
    if running(&exe) {
        return Err(running_error(app));
    }
    recover(app, apps)?;
    let work = work_dir(apps, app);
    let fresh = work.join("new").join(app.name);
    std::fs::create_dir_all(&fresh).map_err(|e| format!("Couldn't create {} ({e})", fresh.display()))?;
    // A folder of that name without the program in it (the person agreed to replace it) is no copy.
    let update = exe.is_file();
    let result = swap_in(app, zip, version, &work, &fresh, &target, version_of);
    // Puts the old copy back if it is still aside, then removes the work folder. If even that fails
    // the work folder stays for the next try: the old copy is never deleted unplaced.
    let cleanup = recover(app, apps);
    result?;
    cleanup?;
    // An update leaves the shortcuts as they are: one the person deleted stays deleted.
    if !update {
        for problem in add_shortcuts(app, &exe, places) {
            log::warn!("{}: {problem}", app.name);
        }
    }
    Ok(Record {
        version: version.to_string(),
        kind: PackageKind::AppZip,
        path: exe,
        installed_at: crate::now(),
        files: Vec::new(),
        needs_root: false,
    })
}

fn swap_in(app: &SuiteApp, zip: &Path, version: &str, work: &Path, fresh: &Path, target: &Path, version_of: VersionOf<'_>) -> Result<(), String> {
    let file = std::fs::File::open(zip).map_err(|e| format!("Couldn't open the package ({e})"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("The package isn't a valid zip ({e})"))?;
    archive.extract(fresh).map_err(|e| format!("Couldn't unpack the package ({e})"))?;
    let new_exe = exe_in(fresh, app);
    if !new_exe.is_file() {
        return Err(format!("The package doesn't contain {}", exe_name(app)));
    }
    let Some(found) = version_of(&new_exe) else {
        return Err(format!("{} in the package doesn't say which version it is", exe_name(app)));
    };
    if version::parse(&found) != version::parse(version) {
        return Err(format!("The package is version {found}, the release says {version}"));
    }
    let backup_parent = work.join("backup");
    let backup = backup_parent.join(app.name);
    let had_old = target.exists();
    if had_old {
        std::fs::create_dir_all(&backup_parent).map_err(|e| format!("Couldn't prepare the update ({e})"))?;
        std::fs::rename(target, &backup).map_err(|e| format!("Couldn't move the installed version aside; is a file in use? ({e})"))?;
    }
    if let Err(e) = std::fs::rename(fresh, target) {
        if had_old {
            let _ = std::fs::rename(&backup, target);
        }
        return Err(format!("Couldn't put the new version in place ({e})"));
    }
    Ok(())
}

/// Removes the app's folder and its shortcuts. The app's settings and data live elsewhere and are
/// kept.
pub fn uninstall(app: &SuiteApp, exe: &Path) -> Result<(), String> {
    remove(app, exe, &Places::system().ok_or("no per-user programs folder on this system")?)
}

/// [`uninstall`] in `places`.
pub(crate) fn remove(app: &SuiteApp, exe: &Path, places: &Places) -> Result<(), String> {
    // The whole app folder goes, also when the program sits in a folder of its own inside it.
    let app_folder = app_dir(app, places);
    let dir = app_folder.as_path();
    if !same_path(exe, &exe_in(dir, app)) {
        let shown = exe.parent().unwrap_or(exe);
        return Err(format!("{} isn't {}'s folder, so it was left alone", shown.display(), app.name));
    }
    if running(exe) {
        return Err(running_error(app));
    }
    let apps = &places.apps;
    recover(app, apps)?;
    // Looked up while the program is still there to compare with; removed once it is gone.
    let shortcuts = own_shortcuts(app, exe, places);
    if dir.exists() {
        // Moved out of the way first, so a failure halfway never leaves half a program in place.
        let work = work_dir(apps, app);
        std::fs::create_dir_all(&work).map_err(|e| format!("Couldn't prepare the removal ({e})"))?;
        std::fs::rename(dir, work.join("removed")).map_err(|e| format!("Couldn't remove {}; is a file in use? ({e})", dir.display()))?;
        std::fs::remove_dir_all(&work).map_err(|e| format!("Couldn't delete {} ({e})", work.display()))?;
    }
    for link in shortcuts {
        if let Err(e) = std::fs::remove_file(&link) {
            log::warn!("{}: couldn't remove {} ({e})", app.name, link.display());
        }
    }
    Ok(())
}

/// Whether two paths are spelled alike: Windows ignores case and the direction of the slashes.
fn same_path(left: &Path, right: &Path) -> bool {
    let plain = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    plain(left) == plain(right)
}

/// Whether two paths name the same file, however each is spelled: a shortcut holds the full names
/// of folders that a path from the environment may give in their short form (`PROGRA~1`).
fn same_file(left: &Path, right: &Path) -> bool {
    let full = |p: &Path| dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    same_path(left, right) || same_path(&full(left), &full(right))
}

fn shortcut_path(folder: &Path, app: &SuiteApp) -> PathBuf {
    folder.join(format!("{}.lnk", app.name))
}

/// Leaves a shortcut to `exe` in each of the shortcut folders, and returns what went wrong: a
/// missing shortcut never fails an install. A shortcut that is already there is never replaced: it
/// is this app's own, or someone's shortcut to another copy.
fn add_shortcuts(app: &SuiteApp, exe: &Path, places: &Places) -> Vec<String> {
    let mut problems = Vec::new();
    for folder in &places.shortcuts {
        let link = shortcut_path(folder, app);
        if std::fs::symlink_metadata(&link).is_ok() {
            continue;
        }
        let made = std::fs::create_dir_all(folder).map_err(|e| format!("couldn't create {} ({e})", folder.display()));
        if let Err(problem) = made.and_then(|()| write_shortcut(&link, exe, app.tagline)) {
            problems.push(problem);
        }
    }
    problems
}

/// The app's shortcuts that point at `exe`: a shortcut of the same name to another copy of the
/// app isn't the launcher's to remove.
fn own_shortcuts(app: &SuiteApp, exe: &Path, places: &Places) -> Vec<PathBuf> {
    places
        .shortcuts
        .iter()
        .map(|folder| shortcut_path(folder, app))
        .filter(|link| shortcut_target(link).is_some_and(|target| same_file(&target, exe)))
        .collect()
}

/// Writes a Windows shortcut at `link` that starts `exe` from its own folder.
fn write_shortcut(link: &Path, exe: &Path, description: &str) -> Result<(), String> {
    use winsafe::prelude::{ole_IPersistFile, ole_IUnknown, shell_IShellLink};
    use winsafe::{CoCreateInstance, CoInitializeEx, IPersistFile, IShellLink, IUnknown, co};
    let failed = |e: co::HRESULT| format!("couldn't create {} ({e})", link.display());
    let text = |p: &Path| p.to_str().map(str::to_string).ok_or_else(|| format!("{} isn't a usable path", p.display()));
    let (link_text, exe_text) = (text(link)?, text(exe)?);
    let _com = CoInitializeEx(co::COINIT::APARTMENTTHREADED | co::COINIT::DISABLE_OLE1DDE).map_err(failed)?;
    let shortcut = CoCreateInstance::<IShellLink>(&co::CLSID::ShellLink, None::<&IUnknown>, co::CLSCTX::INPROC_SERVER).map_err(failed)?;
    shortcut.SetPath(&exe_text).map_err(failed)?;
    if let Some(dir) = exe.parent().and_then(Path::to_str) {
        shortcut.SetWorkingDirectory(dir).map_err(failed)?;
    }
    shortcut.SetDescription(description).map_err(failed)?;
    shortcut.QueryInterface::<IPersistFile>().map_err(failed)?.Save(Some(&link_text), true).map_err(failed)
}

/// The program a Windows shortcut starts, as written in it. `None` when `link` isn't a shortcut.
fn shortcut_target(link: &Path) -> Option<PathBuf> {
    use winsafe::prelude::{ole_IPersistFile, ole_IUnknown, shell_IShellLink};
    use winsafe::{CoCreateInstance, CoInitializeEx, IPersistFile, IShellLink, IUnknown, co};
    if !link.is_file() {
        return None;
    }
    let _com = CoInitializeEx(co::COINIT::APARTMENTTHREADED | co::COINIT::DISABLE_OLE1DDE).ok()?;
    let shortcut = CoCreateInstance::<IShellLink>(&co::CLSID::ShellLink, None::<&IUnknown>, co::CLSCTX::INPROC_SERVER).ok()?;
    shortcut.QueryInterface::<IPersistFile>().ok()?.Load(link.to_str()?, co::STGM::READ).ok()?;
    shortcut.GetPath(None, co::SLGP::RAWPATH).ok().filter(|target| !target.is_empty()).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A sample standalone app with a neutral name.
    fn app() -> SuiteApp {
        let plugin = *SuiteApp::by_id("opentools").expect("OpenTools is in the catalog");
        SuiteApp {
            id: "phototool",
            name: "PhotoTool",
            tagline: "Sample program",
            package: open3d_launcher_core::catalog::AppPackage::Standalone,
            ..plugin
        }
    }

    /// The tests' programs are text files that hold their version: a real exe needs a resource
    /// compiler. [`exe_version`] itself is tested on a program that ships with Windows.
    fn written_version(exe: &Path) -> Option<String> {
        std::fs::read_to_string(exe).ok().map(|text| text.trim().to_string()).filter(|text| !text.is_empty())
    }

    /// A release zip: `exe` (when named) holding `version`, a licence and a file named after `marker`.
    fn package(dir: &Path, exe: Option<&str>, version: &str, marker: &str) -> PathBuf {
        let path = dir.join(format!("phototool-{version}-{marker}.zip"));
        let mut z = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        if let Some(exe) = exe {
            z.start_file(exe, o).unwrap();
            z.write_all(version.as_bytes()).unwrap();
        }
        z.start_file("LICENSE", o).unwrap();
        z.write_all(b"GPL-3.0-or-later").unwrap();
        z.start_file(format!("docs/{marker}.txt"), o).unwrap();
        z.write_all(marker.as_bytes()).unwrap();
        z.finish().unwrap();
        path
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().into_string().unwrap()).collect();
        v.sort();
        v
    }

    fn install_sample(tmp: &Path, places: &Places, version: &str, marker: &str) -> Result<Record, String> {
        install_into(&app(), &package(tmp, Some("PhotoTool.exe"), version, marker), version, places, &written_version)
    }

    fn installed_version(places: &Places) -> Option<String> {
        detect_in(&app(), places, &written_version).and_then(|found| found.version).map(|v| v.to_string())
    }

    #[test]
    fn installs_then_updates_without_leftovers() {
        let tmp = tempfile::tempdir().unwrap();
        let places = Places::under(&tmp.path().join("home"));
        assert_eq!(detect_in(&app(), &places, &written_version), None);
        install_sample(tmp.path(), &places, "0.5.0", "old").unwrap();
        let record = install_sample(tmp.path(), &places, "0.5.1", "new").unwrap();
        let dir = places.apps.join("PhotoTool");
        assert_eq!((record.kind, &record.path), (PackageKind::AppZip, &dir.join("PhotoTool.exe")));
        assert_eq!(names(&dir), ["LICENSE", "PhotoTool.exe", "docs"]);
        assert_eq!(names(&dir.join("docs")), ["new.txt"], "old files must not survive an update");
        assert_eq!(names(&places.apps), ["PhotoTool"]);
        let found = detect_in(&app(), &places, &written_version).unwrap();
        assert_eq!(found.version, version::parse("0.5.1"));
        assert_eq!((found.source, found.location.as_str()), (InstallSource::AppFolder, record.path.to_str().unwrap()));
    }

    #[test]
    fn a_first_install_adds_shortcuts_and_an_update_leaves_them_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let places = Places::under(&tmp.path().join("home"));
        let record = install_sample(tmp.path(), &places, "0.5.0", "old").unwrap();
        let links: Vec<PathBuf> = places.shortcuts.iter().map(|folder| folder.join("PhotoTool.lnk")).collect();
        assert_eq!(links.len(), 2);
        for link in &links {
            let target = shortcut_target(link).unwrap_or_else(|| panic!("no shortcut at {}", link.display()));
            assert!(same_file(&target, &record.path), "{} points at {}", link.display(), target.display());
        }
        // The person threw the desktop shortcut away: the update doesn't bring it back.
        std::fs::remove_file(&links[1]).unwrap();
        install_sample(tmp.path(), &places, "0.5.1", "new").unwrap();
        assert!(links[0].is_file() && !links[1].exists());
    }

    #[test]
    fn a_bad_package_keeps_the_installed_version() {
        let tmp = tempfile::tempdir().unwrap();
        let places = Places::under(&tmp.path().join("home"));
        let a = app();
        install_sample(tmp.path(), &places, "0.5.0", "old").unwrap();
        let try_install = |zip: &Path| install_into(&a, zip, "0.5.1", &places, &written_version).unwrap_err();
        assert!(try_install(&package(tmp.path(), None, "0.5.1", "empty")).contains("doesn't contain PhotoTool.exe"));
        assert!(try_install(&package(tmp.path(), Some("Other.exe"), "0.5.1", "other")).contains("doesn't contain PhotoTool.exe"));
        // The program must be at the zip's root, not in a folder of its own.
        assert!(try_install(&package(tmp.path(), Some("PhotoTool/PhotoTool.exe"), "0.5.1", "nested")).contains("doesn't contain"));
        assert!(try_install(&package(tmp.path(), Some("PhotoTool.exe"), "0.6.0", "wrong")).contains("release says"));
        assert!(try_install(&package(tmp.path(), Some("PhotoTool.exe"), "", "silent")).contains("doesn't say which version"));
        let not_a_zip = tmp.path().join("broken.zip");
        std::fs::write(&not_a_zip, b"not a zip").unwrap();
        assert!(try_install(&not_a_zip).contains("valid zip"));
        assert_eq!(installed_version(&places).as_deref(), Some("0.5.0"));
        assert!(places.apps.join("PhotoTool").join("docs").join("old.txt").is_file());
        assert_eq!(names(&places.apps), ["PhotoTool"]);
    }

    #[test]
    fn installing_over_a_folder_without_the_program_is_a_first_install() {
        let tmp = tempfile::tempdir().unwrap();
        let places = Places::under(&tmp.path().join("home"));
        let dir = places.apps.join("PhotoTool");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), b"something else").unwrap();
        install_sample(tmp.path(), &places, "0.5.0", "new").unwrap();
        assert_eq!(names(&dir), ["LICENSE", "PhotoTool.exe", "docs"]);
        assert_eq!(names(&places.apps), ["PhotoTool"]);
        assert!(places.shortcuts.iter().all(|folder| folder.join("PhotoTool.lnk").is_file()));
    }

    #[test]
    fn a_failed_first_install_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let places = Places::under(&tmp.path().join("home"));
        assert!(install_into(&app(), &package(tmp.path(), None, "0.5.0", "empty"), "0.5.0", &places, &written_version).is_err());
        assert!(names(&places.apps).is_empty());
        assert!(places.shortcuts.iter().all(|folder| !folder.join("PhotoTool.lnk").exists()));
    }

    #[test]
    fn recovers_from_an_interrupted_update() {
        let tmp = tempfile::tempdir().unwrap();
        let places = Places::under(&tmp.path().join("home"));
        install_sample(tmp.path(), &places, "0.5.0", "old").unwrap();
        // The launcher stopped between moving the old copy aside and putting the new one in place.
        let work = places.apps.join(".open3d-phototool");
        std::fs::create_dir_all(work.join("backup")).unwrap();
        std::fs::create_dir_all(work.join("new").join("PhotoTool")).unwrap();
        std::fs::rename(places.apps.join("PhotoTool"), work.join("backup").join("PhotoTool")).unwrap();
        assert_eq!(installed_version(&places), None);
        recover(&app(), &places.apps).unwrap();
        assert_eq!(installed_version(&places).as_deref(), Some("0.5.0"));
        assert_eq!(names(&places.apps), ["PhotoTool"]);
    }

    #[test]
    fn nothing_is_replaced_or_removed_while_the_program_runs() {
        use std::os::windows::fs::OpenOptionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let places = Places::under(&tmp.path().join("home"));
        let record = install_sample(tmp.path(), &places, "0.5.0", "old").unwrap();
        assert!(!running(&record.path));
        assert!(!running(&places.apps.join("Missing").join("Missing.exe")));
        // Windows holds a running program's file the way this handle does: readable, not writable.
        let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&record.path).unwrap();
        assert!(running(&record.path));
        assert_eq!(install_sample(tmp.path(), &places, "0.5.1", "new").unwrap_err(), "PhotoTool is running. Quit it, then try again.");
        assert_eq!(remove(&app(), &record.path, &places).unwrap_err(), "PhotoTool is running. Quit it, then try again.");
        assert_eq!(installed_version(&places).as_deref(), Some("0.5.0"));
        assert_eq!(names(&places.apps), ["PhotoTool"]);
        drop(held);
        install_sample(tmp.path(), &places, "0.5.1", "new").unwrap();
        assert_eq!(installed_version(&places).as_deref(), Some("0.5.1"));
    }

    #[test]
    fn uninstall_removes_only_the_app_and_its_own_shortcuts() {
        let tmp = tempfile::tempdir().unwrap();
        let places = Places::under(&tmp.path().join("home"));
        let a = app();
        let record = install_sample(tmp.path(), &places, "0.5.1", "x").unwrap();
        // Another app's folder, and a desktop shortcut of the same name to another copy of the app.
        let other = places.apps.join("Launcher");
        std::fs::create_dir_all(&other).unwrap();
        let elsewhere = tmp.path().join("dev").join("PhotoTool.exe");
        std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
        std::fs::write(&elsewhere, b"9.9.9").unwrap();
        let desktop_link = places.shortcuts[1].join("PhotoTool.lnk");
        std::fs::remove_file(&desktop_link).unwrap();
        write_shortcut(&desktop_link, &elsewhere, "Development copy").unwrap();

        assert!(remove(&a, &elsewhere, &places).unwrap_err().contains("left alone"));
        assert!(remove(&a, &other.join("PhotoTool.exe"), &places).unwrap_err().contains("left alone"));
        assert!(elsewhere.is_file() && other.is_dir());

        remove(&a, &record.path, &places).unwrap();
        assert_eq!(names(&places.apps), ["Launcher"]);
        assert!(!places.shortcuts[0].join("PhotoTool.lnk").exists(), "the app's own shortcut stays behind");
        assert!(desktop_link.is_file(), "a shortcut to another copy was removed");
        assert_eq!(detect_in(&a, &places, &written_version), None);
    }

    #[test]
    fn a_copy_put_there_by_hand_is_found_and_one_without_a_version_is_still_a_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let places = Places::under(tmp.path());
        let dir = places.apps.join("PhotoTool");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(detect_in(&app(), &places, &written_version), None, "a folder without the program isn't a copy");
        std::fs::write(dir.join("PhotoTool.exe"), b"0.4.0").unwrap();
        assert_eq!(installed_version(&places).as_deref(), Some("0.4.0"));
        std::fs::write(dir.join("PhotoTool.exe"), b"").unwrap();
        let found = detect_in(&app(), &places, &written_version).unwrap();
        assert_eq!(found.version, None);
    }

    #[test]
    fn a_name_windows_refuses_is_never_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let places = Places::under(&tmp.path().join("home"));
        let odd = SuiteApp { name: "Photo:Tool", ..app() };
        let zip = package(tmp.path(), Some("PhotoTool.exe"), "0.5.0", "x");
        assert!(install_into(&odd, &zip, "0.5.0", &places, &written_version).unwrap_err().contains("folder's name"));
        assert!(!places.apps.exists());
    }

    /// The sample app with its program in `bin`, next to folders it needs (OpenGaussian's layout).
    fn nested_app() -> SuiteApp {
        SuiteApp { exe: Some("bin/PhotoTool.exe"), ..app() }
    }

    #[test]
    fn a_program_in_a_folder_of_its_own_is_installed_found_and_removed_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let places = Places::under(&tmp.path().join("home"));
        let a = nested_app();
        let dir = places.apps.join("PhotoTool");
        assert_eq!(exe_in(&dir, &a), dir.join("bin").join("PhotoTool.exe"));

        // The program must be where the catalog says: one at the root isn't it.
        let at_root = package(tmp.path(), Some("PhotoTool.exe"), "0.1.0", "root");
        assert!(install_into(&a, &at_root, "0.1.0", &places, &written_version).unwrap_err().contains("doesn't contain bin/PhotoTool.exe"));

        let record = install_into(&a, &package(tmp.path(), Some("bin/PhotoTool.exe"), "0.1.0", "old"), "0.1.0", &places, &written_version).unwrap();
        assert_eq!(record.path, dir.join("bin").join("PhotoTool.exe"));
        assert_eq!(names(&dir), ["LICENSE", "bin", "docs"]);
        for folder in &places.shortcuts {
            let target = shortcut_target(&folder.join("PhotoTool.lnk")).unwrap();
            assert!(same_file(&target, &record.path), "{}", target.display());
        }
        let found = detect_in(&a, &places, &written_version).unwrap();
        assert_eq!(found.version, version::parse("0.1.0"));
        assert_eq!(found.location, record.path.to_string_lossy());
        // Not found as the plain layout, and the plain layout's program isn't this one.
        assert_eq!(detect_in(&app(), &places, &written_version), None);

        install_into(&a, &package(tmp.path(), Some("bin/PhotoTool.exe"), "0.2.0", "new"), "0.2.0", &places, &written_version).unwrap();
        assert_eq!(names(&dir.join("docs")), ["new.txt"]);

        // A program of the same name elsewhere in the folder isn't the app's.
        assert!(remove(&a, &dir.join("PhotoTool.exe"), &places).unwrap_err().contains("left alone"));
        remove(&a, &record.path, &places).unwrap();
        assert!(names(&places.apps).is_empty(), "the whole app folder goes, not only bin");
        assert!(places.shortcuts.iter().all(|folder| !folder.join("PhotoTool.lnk").exists()));
    }

    #[test]
    fn reads_the_version_from_a_programs_file_properties() {
        let system = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot is set on Windows"));
        let found = exe_version(&system.join("System32").join("cmd.exe")).expect("cmd.exe says its version");
        let parsed = version::parse(&found).unwrap_or_else(|| panic!("{found} isn't a version"));
        assert!(parsed.major >= 6, "{found}");
        let tmp = tempfile::tempdir().unwrap();
        let text = tmp.path().join("NotAProgram.exe");
        std::fs::write(&text, b"0.5.0").unwrap();
        assert_eq!(exe_version(&text), None);
        assert_eq!(exe_version(&tmp.path().join("missing.exe")), None);
    }

    #[test]
    fn the_test_folders_mirror_the_real_layout() {
        let places = Places::under(Path::new(r"C:\scratch"));
        assert_eq!(places.apps, Path::new(r"C:\scratch\Programs\Open3D"));
        assert_eq!(app_dir(&app(), &places), Path::new(r"C:\scratch\Programs\Open3D\PhotoTool"));
        assert_eq!(exe_name(&app()), "PhotoTool.exe");
        assert!(same_path(Path::new(r"C:\Scratch\A\b.exe"), Path::new("c:/scratch/a/B.EXE")));
        assert!(!same_path(Path::new(r"C:\scratch\a.exe"), Path::new(r"C:\scratch\b.exe")));
    }
}
