// SPDX-License-Identifier: GPL-3.0-or-later
//! Open3D apps are 3ds Max plug-in bundles: installing, updating, finding and removing them.
//!
//! A release asset (`<app>-<version>-windows-x64-3dsmax.zip`) holds the bundle folder at its root
//! (`OpenTools.bundle/PackageContents.xml`, ...). It goes in `%APPDATA%\Autodesk\ApplicationPlugins`,
//! the per-user folder every 3ds Max from 2025 on reads, so no administrator rights are needed.
//!
//! Work happens in `.open3d-<id>` beside the bundles: 3ds Max only looks for `PackageContents.xml`
//! one level down, so the new copy (`new\<bundle>`) and the old one (`backup\<bundle>`) are never
//! loaded, even if the launcher stops halfway; [`recover`] puts a backup back on the next start.
//!
//! Open3D's own code (from Open3D Launcher 0.1), not part of ArtCraft Launcher.

use std::path::{Path, PathBuf};

use open3d_launcher_core::{InstallSource, Installed, SuiteApp, version};

use crate::manifest::Record;

/// `%APPDATA%\Autodesk\ApplicationPlugins`.
pub fn plugins_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Autodesk").join("ApplicationPlugins"))
}

/// The bundle folder's name: `<App name>.bundle` (`OpenTools.bundle`).
pub fn bundle_name(app: &SuiteApp) -> String {
    format!("{}.bundle", app.name)
}

/// Where this app's bundle lives.
pub fn bundle_dir(app: &SuiteApp) -> Option<PathBuf> {
    plugins_dir().map(|p| p.join(bundle_name(app)))
}

/// Value of an XML attribute in the first tag that has it (enough for `PackageContents.xml`).
pub fn xml_attr(text: &str, name: &str) -> Option<String> {
    let mut rest = text;
    while let Some(i) = rest.find(name) {
        let before_ok = i == 0 || rest[..i].ends_with(|c: char| c.is_whitespace());
        let after = rest[i + name.len()..].trim_start();
        if before_ok && let Some(eq) = after.strip_prefix('=') {
            let eq = eq.trim_start();
            if let Some(q) = eq.chars().next().filter(|c| *c == '"' || *c == '\'') {
                let body = &eq[1..];
                return body.find(q).map(|end| body[..end].to_string());
            }
        }
        rest = &rest[i + name.len()..];
    }
    None
}

/// `AppVersion` of a bundle folder.
pub fn bundle_version(dir: &Path) -> Option<String> {
    xml_attr(&std::fs::read_to_string(dir.join("PackageContents.xml")).ok()?, "AppVersion")
}

/// The installed bundle, if there is one. A bundle the launcher didn't install (copied by hand) is
/// found too: it can be updated and removed the same way.
pub fn detect(app: &SuiteApp) -> Option<Installed> {
    let dir = bundle_dir(app)?;
    if !dir.join("PackageContents.xml").is_file() {
        return None;
    }
    Some(Installed {
        version: bundle_version(&dir).as_deref().and_then(version::parse),
        source: InstallSource::Bundle,
        location: dir.to_string_lossy().into_owned(),
        package: None,
        externally_managed: false,
        origin: None,
        installation: None,
    })
}

/// Other bundles of the same app in the plug-in folder (`OpenTools-dev.bundle`): 3ds Max loads only
/// the first one it finds.
pub fn other_copies(app: &SuiteApp) -> Vec<String> {
    plugins_dir().map(|plugins| other_copies_in(app, &plugins)).unwrap_or_default()
}

/// [other_copies] in the plug-in folder `plugins`, sorted by name.
pub(crate) fn other_copies_in(app: &SuiteApp, plugins: &Path) -> Vec<String> {
    let stem = app.name.to_ascii_lowercase();
    let own = bundle_name(app).to_ascii_lowercase();
    let Ok(entries) = std::fs::read_dir(plugins) else { return Vec::new() };
    let mut found: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l != own && l.starts_with(&stem) && l.ends_with(".bundle")
        })
        .collect();
    found.sort();
    found
}

/// True while any 3ds Max is running: its files are in use and it caches the bundles it loaded.
pub fn max_running() -> bool {
    // Development builds only: test installs while 3ds Max is open (the release build ignores it).
    if cfg!(debug_assertions) && std::env::var_os("OPEN3D_LAUNCHER_IGNORE_MAX").is_some() {
        return false;
    }
    #[cfg(windows)]
    {
        let out = crate::process::command("tasklist").args(["/FI", "IMAGENAME eq 3dsmax.exe", "/NH", "/FO", "CSV"]).output();
        out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).to_ascii_lowercase().contains("\"3dsmax.exe\""))
    }
}

/// The error for anything that must wait until 3ds Max is closed.
pub fn max_running_error() -> String {
    "3ds Max is running. Close it, then try again.".into()
}

fn work_dir(plugins: &Path, app: &SuiteApp) -> PathBuf {
    plugins.join(format!(".open3d-{}", app.id))
}

/// Puts back an old copy left aside by an interrupted update, then removes the work folder.
pub fn recover(app: &SuiteApp, plugins: &Path) -> Result<(), String> {
    let work = work_dir(plugins, app);
    if !work.exists() {
        return Ok(());
    }
    let target = plugins.join(bundle_name(app));
    let backup = work.join("backup").join(bundle_name(app));
    if backup.is_dir() && !target.exists() {
        std::fs::rename(&backup, &target).map_err(|e| format!("Couldn't restore {} ({e})", target.display()))?;
    }
    std::fs::remove_dir_all(&work).map_err(|e| format!("Couldn't clean {} ({e})", work.display()))
}

/// Installs or updates `app` from the downloaded `zip`; the package must say it is `version`.
pub fn install(app: &SuiteApp, zip: &Path, version: &str) -> Result<Record, String> {
    if max_running() {
        return Err(max_running_error());
    }
    install_into(app, zip, version, &plugins_dir().ok_or("APPDATA is not set")?)
}

/// [install] into plugins, without the 3ds Max check (the caller has done it).
pub(crate) fn install_into(app: &SuiteApp, zip: &Path, version: &str, plugins: &Path) -> Result<Record, String> {
    std::fs::create_dir_all(plugins).map_err(|e| format!("Couldn't create {} ({e})", plugins.display()))?;
    recover(app, plugins)?;
    let work = work_dir(plugins, app);
    let fresh = work.join("new");
    std::fs::create_dir_all(&fresh).map_err(|e| format!("Couldn't create {} ({e})", fresh.display()))?;
    let target = plugins.join(bundle_name(app));
    let result = swap_in(app, zip, version, &work, &fresh, &target);
    // Puts the old copy back if it is still aside, then removes the work folder. If even that fails
    // the work folder stays for the next start: the old copy is never deleted unplaced.
    let cleanup = recover(app, plugins);
    result?;
    cleanup?;
    Ok(Record {
        version: version.to_string(),
        kind: open3d_launcher_core::PackageKind::MaxBundle,
        path: target,
        installed_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64),
        files: Vec::new(),
        needs_root: false,
    })
}

fn swap_in(app: &SuiteApp, zip: &Path, version: &str, work: &Path, fresh: &Path, target: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip).map_err(|e| format!("Couldn't open the package ({e})"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("The package isn't a valid zip ({e})"))?;
    archive.extract(fresh).map_err(|e| format!("Couldn't unpack the package ({e})"))?;
    let new_bundle = fresh.join(bundle_name(app));
    let Some(found) = bundle_version(&new_bundle) else {
        return Err(format!("The package doesn't contain {}/PackageContents.xml", bundle_name(app)));
    };
    if version::parse(&found) != version::parse(version) {
        return Err(format!("The package is version {found}, the release says {version}"));
    }
    let backup_parent = work.join("backup");
    let backup = backup_parent.join(bundle_name(app));
    let had_old = target.exists();
    if had_old {
        std::fs::create_dir_all(&backup_parent).map_err(|e| format!("Couldn't prepare the update ({e})"))?;
        std::fs::rename(target, &backup).map_err(|e| format!("Couldn't move the installed version aside; is a file in use? ({e})"))?;
    }
    if let Err(e) = std::fs::rename(&new_bundle, target) {
        if had_old {
            let _ = std::fs::rename(&backup, target);
        }
        return Err(format!("Couldn't put the new version in place ({e})"));
    }
    Ok(())
}

/// Removes the bundle folder. The app's settings and data live elsewhere and are kept.
pub fn uninstall(app: &SuiteApp, dir: &Path) -> Result<(), String> {
    if max_running() {
        return Err(max_running_error());
    }
    remove(app, dir)
}

/// [uninstall] without the 3ds Max check.
fn remove(app: &SuiteApp, dir: &Path) -> Result<(), String> {
    let plugins = dir.parent().ok_or("the bundle has no parent folder")?;
    if dir.file_name().and_then(|n| n.to_str()) != Some(bundle_name(app).as_str()) {
        return Err(format!("{} isn't {}'s bundle, so it was left alone", dir.display(), app.name));
    }
    recover(app, plugins)?;
    if dir.exists() {
        // Moved out of the way first, so a failure halfway never leaves a broken bundle Max would load.
        let work = work_dir(plugins, app);
        std::fs::create_dir_all(&work).map_err(|e| format!("Couldn't prepare the removal ({e})"))?;
        std::fs::rename(dir, work.join("removed")).map_err(|e| format!("Couldn't remove {}; is a file in use? ({e})", dir.display()))?;
        std::fs::remove_dir_all(&work).map_err(|e| format!("Couldn't delete {} ({e})", work.display()))?;
    }
    Ok(())
}

/// The newest 3ds Max installed (`...\3dsmax.exe`), from its registry entries.
#[cfg(windows)]
pub fn newest_max() -> Option<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::HKEY_LOCAL_MACHINE;
    let root = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(r"SOFTWARE\Autodesk\3dsMax").ok()?;
    let mut best: Option<(u32, PathBuf)> = None;
    for name in root.enum_keys().flatten() {
        let Some(major) = name.split('.').next().and_then(|m| m.parse::<u32>().ok()) else { continue };
        let Ok(key) = root.open_subkey(&name) else { continue };
        let Ok(dir) = key.get_value::<String, _>("Installdir") else { continue };
        let exe = PathBuf::from(dir).join("3dsmax.exe");
        if exe.is_file() && best.as_ref().is_none_or(|(m, _)| major > *m) {
            best = Some((major, exe));
        }
    }
    best.map(|(_, exe)| exe)
}

/// "Open" for a plug-in: start the newest 3ds Max, which loads the bundle.
pub fn launch(app: &SuiteApp) -> Result<(), String> {
    #[cfg(windows)]
    if let Some(max) = newest_max() {
        return crate::process::launch_binary(&max, &[]);
    }
    Err(format!("{} runs inside 3ds Max. Open 3ds Max to use it.", app.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn app() -> SuiteApp {
        *open3d_launcher_core::SuiteApp::by_id("opentools").expect("OpenTools is in the catalog")
    }

    fn package(dir: &Path, bundle: &str, version: &str, marker: &str) -> PathBuf {
        let path = dir.join(format!("{bundle}-{version}-{marker}.zip"));
        let mut z = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        z.start_file(format!("{bundle}/PackageContents.xml"), o).unwrap();
        write!(z, r#"<ApplicationPackage SchemaVersion="1.0" AppVersion="{version}"><Components/></ApplicationPackage>"#).unwrap();
        z.start_file(format!("{bundle}/Contents/{marker}.txt"), o).unwrap();
        z.write_all(marker.as_bytes()).unwrap();
        z.finish().unwrap();
        path
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().into_string().unwrap()).collect();
        v.sort();
        v
    }

    #[test]
    fn reads_app_version_not_component_version() {
        let xml = r#"<ApplicationPackage Name="x" AppVersion="0.5.1"><ComponentEntry Version="9.9.9"/></ApplicationPackage>"#;
        assert_eq!(xml_attr(xml, "AppVersion").as_deref(), Some("0.5.1"));
        assert_eq!(xml_attr("<a AppVersion = '1.2' />", "AppVersion").as_deref(), Some("1.2"));
    }

    #[test]
    fn installs_then_updates_without_leftovers() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let a = app();
        install_into(&a, &package(tmp.path(), "OpenTools.bundle", "0.5.0", "old"), "0.5.0", &plugins).unwrap();
        let record = install_into(&a, &package(tmp.path(), "OpenTools.bundle", "0.5.1", "new"), "0.5.1", &plugins).unwrap();
        assert_eq!(record.path, plugins.join("OpenTools.bundle"));
        assert_eq!(bundle_version(&record.path).as_deref(), Some("0.5.1"));
        assert!(record.path.join("Contents/new.txt").exists());
        assert!(!record.path.join("Contents/old.txt").exists(), "old files must not survive an update");
        assert_eq!(names(&plugins), ["OpenTools.bundle"]);
    }

    #[test]
    fn a_bad_package_keeps_the_installed_version() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let a = app();
        install_into(&a, &package(tmp.path(), "OpenTools.bundle", "0.5.0", "old"), "0.5.0", &plugins).unwrap();
        let wrong_folder = package(tmp.path(), "Other.bundle", "0.5.1", "x");
        assert!(install_into(&a, &wrong_folder, "0.5.1", &plugins).unwrap_err().contains("doesn't contain"));
        let wrong_version = package(tmp.path(), "OpenTools.bundle", "0.6.0", "y");
        assert!(install_into(&a, &wrong_version, "0.5.1", &plugins).unwrap_err().contains("release says"));
        assert_eq!(bundle_version(&plugins.join("OpenTools.bundle")).as_deref(), Some("0.5.0"));
        assert_eq!(names(&plugins), ["OpenTools.bundle"]);
    }

    #[test]
    fn recovers_from_an_interrupted_update() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let a = app();
        install_into(&a, &package(tmp.path(), "OpenTools.bundle", "0.5.0", "old"), "0.5.0", &plugins).unwrap();
        let backup = plugins.join(".open3d-opentools").join("backup");
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::rename(plugins.join("OpenTools.bundle"), backup.join("OpenTools.bundle")).unwrap();
        recover(&a, &plugins).unwrap();
        assert_eq!(bundle_version(&plugins.join("OpenTools.bundle")).as_deref(), Some("0.5.0"));
        assert_eq!(names(&plugins), ["OpenTools.bundle"]);
    }

    #[test]
    fn finds_other_copies_of_the_app_only() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path();
        let a = app();
        assert!(other_copies_in(&a, &plugins.join("missing")).is_empty());
        for dir in ["OpenTools.bundle", "opentools-dev.bundle", "OpenTools-old.bundle", "OpenManager.bundle", "VRay3dsMax2027", ".open3d-opentools"] {
            std::fs::create_dir_all(plugins.join(dir)).unwrap();
        }
        // A file with a bundle's name is not a bundle.
        std::fs::write(plugins.join("OpenTools-copy.bundle"), b"").unwrap();
        assert_eq!(other_copies_in(&a, plugins), ["OpenTools-old.bundle", "opentools-dev.bundle"]);
        // Found whether or not the app itself is installed: installing it would make two copies.
        std::fs::remove_dir_all(plugins.join("OpenTools.bundle")).unwrap();
        assert_eq!(other_copies_in(&a, plugins), ["OpenTools-old.bundle", "opentools-dev.bundle"]);
    }

    #[test]
    fn uninstall_removes_only_the_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let a = app();
        install_into(&a, &package(tmp.path(), "OpenTools.bundle", "0.5.1", "x"), "0.5.1", &plugins).unwrap();
        std::fs::create_dir_all(plugins.join("VRay3dsMax2027")).unwrap();
        remove(&a, &plugins.join("OpenTools.bundle")).unwrap();
        assert_eq!(names(&plugins), ["VRay3dsMax2027"]);
        assert!(remove(&a, &plugins.join("VRay3dsMax2027")).unwrap_err().contains("left alone"));
    }
}
