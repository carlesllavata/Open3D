// SPDX-License-Identifier: GPL-3.0-or-later
//! Installing, updating and removing app bundles in the ApplicationPlugins folder.
//!
//! A package is a zip whose root holds the bundle folder (`OpenTools.bundle/PackageContents.xml`, ...).
//! Work happens in `.open3d-<id>` next to the bundles: 3ds Max only looks for `PackageContents.xml` one level
//! down, so the new copy (`new/<bundle>`) and the old one (`backup/<bundle>`) sitting two levels down are never
//! loaded, even if the launcher is killed halfway.

use crate::catalog::{AppEntry, valid_bundle_name};
use std::fs;
use std::path::{Path, PathBuf};

/// `%APPDATA%\Autodesk\ApplicationPlugins`, the per-user plugin folder all 3ds Max versions read.
pub fn default_plugins_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Autodesk").join("ApplicationPlugins"))
}

fn work_dir(plugins: &Path, app: &AppEntry) -> PathBuf {
    plugins.join(format!(".open3d-{}", app.id))
}

fn target(plugins: &Path, app: &AppEntry) -> Result<PathBuf, String> {
    if !valid_bundle_name(&app.bundle) {
        return Err(format!("Invalid bundle name {:?}", app.bundle));
    }
    Ok(plugins.join(&app.bundle))
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

/// `AppVersion` of a bundle folder, if it is a bundle.
pub fn bundle_version(bundle_dir: &Path) -> Option<String> {
    let xml = fs::read_to_string(bundle_dir.join("PackageContents.xml")).ok()?;
    xml_attr(&xml, "AppVersion")
}

/// Installed version, or `None` when the app is not installed.
pub fn installed_version(plugins: &Path, app: &AppEntry) -> Option<String> {
    let t = target(plugins, app).ok()?;
    if !t.is_dir() {
        return None;
    }
    Some(bundle_version(&t).unwrap_or_else(|| "unknown".to_string()))
}

/// Other bundles of the same app (`OpenTools-dev.bundle`): 3ds Max loads only the first one it finds.
pub fn other_copies(plugins: &Path, app: &AppEntry) -> Vec<String> {
    let stem = app.bundle[..app.bundle.len() - ".bundle".len()].to_ascii_lowercase();
    let own = app.bundle.to_ascii_lowercase();
    let Ok(entries) = fs::read_dir(plugins) else {
        return Vec::new();
    };
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

/// Puts back an old copy left in the work folder by an interrupted update, then removes the work folder.
pub fn recover(plugins: &Path, app: &AppEntry) -> Result<(), String> {
    let work = work_dir(plugins, app);
    if !work.exists() {
        return Ok(());
    }
    let t = target(plugins, app)?;
    let backup = work.join("backup").join(&app.bundle);
    if backup.is_dir() && !t.exists() {
        fs::rename(&backup, &t).map_err(|e| format!("Could not restore {}: {e}", t.display()))?;
    }
    fs::remove_dir_all(&work).map_err(|e| format!("Could not clean {}: {e}", work.display()))
}

/// Installs or updates `app` from a downloaded `zip`. `expected_version` must match the package.
pub fn install(plugins: &Path, app: &AppEntry, zip: &Path, expected_version: &str) -> Result<(), String> {
    let t = target(plugins, app)?;
    fs::create_dir_all(plugins).map_err(|e| format!("Could not create {}: {e}", plugins.display()))?;
    recover(plugins, app)?;

    let work = work_dir(plugins, app);
    let fresh = work.join("new");
    fs::create_dir_all(&fresh).map_err(|e| format!("Could not create {}: {e}", fresh.display()))?;
    let result = install_from_work(&t, app, zip, expected_version, &work, &fresh);
    // `recover` puts the old copy back if it is still aside, then removes the work folder. If even that
    // fails, the work folder stays and the next start tries again: the old copy is never deleted unplaced.
    let cleanup = recover(plugins, app);
    match result {
        Err(e) => Err(e),
        Ok(()) => cleanup,
    }
}

fn install_from_work(
    t: &Path,
    app: &AppEntry,
    zip: &Path,
    expected_version: &str,
    work: &Path,
    fresh: &Path,
) -> Result<(), String> {
    let file = fs::File::open(zip).map_err(|e| format!("Could not open the package: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("The package is not a valid zip: {e}"))?;
    archive
        .extract(fresh)
        .map_err(|e| format!("Could not unpack the package: {e}"))?;

    let new_bundle = fresh.join(&app.bundle);
    let Some(version) = bundle_version(&new_bundle) else {
        return Err(format!(
            "The package does not contain {}/PackageContents.xml",
            app.bundle
        ));
    };
    if crate::catalog::compare_versions(&version, expected_version) != std::cmp::Ordering::Equal {
        return Err(format!(
            "The package is version {version}, the catalog says {expected_version}"
        ));
    }

    let backup_parent = work.join("backup");
    let backup = backup_parent.join(&app.bundle);
    let had_old = t.exists();
    if had_old {
        fs::create_dir_all(&backup_parent).map_err(|e| format!("Could not prepare the update: {e}"))?;
        fs::rename(t, &backup)
            .map_err(|e| format!("Could not move the installed version aside (is a file in use?): {e}"))?;
    }
    if let Err(e) = fs::rename(&new_bundle, t) {
        if had_old {
            let _ = fs::rename(&backup, t);
        }
        return Err(format!("Could not put the new version in place: {e}"));
    }
    Ok(())
}

/// Removes the bundle folder. The app's settings and user data live elsewhere and are kept.
pub fn uninstall(plugins: &Path, app: &AppEntry) -> Result<(), String> {
    let t = target(plugins, app)?;
    recover(plugins, app)?;
    if t.exists() {
        // Move it out of the way first so a failure halfway never leaves a broken bundle that Max would load.
        let work = work_dir(plugins, app);
        fs::create_dir_all(&work).map_err(|e| format!("Could not prepare the removal: {e}"))?;
        let gone = work.join("removed");
        fs::rename(&t, &gone)
            .map_err(|e| format!("Could not remove {} (is a file in use?): {e}", t.display()))?;
        fs::remove_dir_all(&work).map_err(|e| format!("Could not delete {}: {e}", work.display()))?;
    }
    Ok(())
}

/// True while any 3ds Max is running (its files are in use and it caches the bundles it loaded).
pub fn max_running() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq 3dsmax.exe", "/NH", "/FO", "CSV"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout)
                .to_ascii_lowercase()
                .contains("\"3dsmax.exe\""),
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Release;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!("open3d-test-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn app() -> AppEntry {
        AppEntry {
            id: "opentools".into(),
            name: "OpenTools".into(),
            summary: String::new(),
            bundle: "OpenTools.bundle".into(),
            max_min: None,
            latest: Release {
                version: "0.5.0".into(),
                date: String::new(),
                url: "x.zip".into(),
                sha256: "0".repeat(64),
                size: 0,
                notes: String::new(),
            },
        }
    }

    fn package(dir: &Path, bundle: &str, version: &str, marker: &str) -> PathBuf {
        let path = dir.join(format!("{bundle}-{version}.zip"));
        let mut z = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        let o = SimpleFileOptions::default();
        z.start_file(format!("{bundle}/PackageContents.xml"), o).unwrap();
        write!(z, r#"<ApplicationPackage SchemaVersion="1.0" AppVersion="{version}"><Components/></ApplicationPackage>"#).unwrap();
        z.start_file(format!("{bundle}/Contents/{marker}.txt"), o)
            .unwrap();
        z.write_all(marker.as_bytes()).unwrap();
        z.finish().unwrap();
        path
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn reads_app_version_not_component_version() {
        let xml = r#"<ApplicationPackage Name="x" AppVersion="0.5.0"><ComponentEntry Version="9.9.9"/></ApplicationPackage>"#;
        assert_eq!(xml_attr(xml, "AppVersion").as_deref(), Some("0.5.0"));
        assert_eq!(xml_attr(xml, "Version").as_deref(), Some("9.9.9"));
        assert_eq!(
            xml_attr("<a AppVersion = '1.2' />", "AppVersion").as_deref(),
            Some("1.2")
        );
    }

    #[test]
    fn installs_then_updates_without_leftovers() {
        let s = Scratch::new("update");
        let plugins = s.0.join("plugins");
        let a = app();
        install(
            &plugins,
            &a,
            &package(&s.0, "OpenTools.bundle", "0.4.0", "old"),
            "0.4.0",
        )
        .unwrap();
        assert_eq!(installed_version(&plugins, &a).as_deref(), Some("0.4.0"));
        install(
            &plugins,
            &a,
            &package(&s.0, "OpenTools.bundle", "0.5.0", "new"),
            "0.5.0",
        )
        .unwrap();
        assert_eq!(installed_version(&plugins, &a).as_deref(), Some("0.5.0"));
        let contents = plugins.join("OpenTools.bundle").join("Contents");
        assert!(contents.join("new.txt").exists());
        assert!(
            !contents.join("old.txt").exists(),
            "old files must not survive an update"
        );
        assert_eq!(names(&plugins), vec!["OpenTools.bundle"]);
    }

    #[test]
    fn a_bad_package_keeps_the_installed_version() {
        let s = Scratch::new("bad");
        let plugins = s.0.join("plugins");
        let a = app();
        install(
            &plugins,
            &a,
            &package(&s.0, "OpenTools.bundle", "0.4.0", "old"),
            "0.4.0",
        )
        .unwrap();
        let wrong_folder = package(&s.0, "Other.bundle", "0.5.0", "x");
        assert!(
            install(&plugins, &a, &wrong_folder, "0.5.0")
                .unwrap_err()
                .contains("does not contain")
        );
        let wrong_version = package(&s.0, "OpenTools.bundle", "0.6.0", "x");
        assert!(
            install(&plugins, &a, &wrong_version, "0.5.0")
                .unwrap_err()
                .contains("catalog says")
        );
        let not_zip = s.0.join("not.zip");
        fs::write(&not_zip, b"nope").unwrap();
        assert!(install(&plugins, &a, &not_zip, "0.5.0").is_err());
        assert_eq!(installed_version(&plugins, &a).as_deref(), Some("0.4.0"));
        assert_eq!(names(&plugins), vec!["OpenTools.bundle"]);
    }

    #[test]
    fn recovers_from_an_interrupted_update() {
        let s = Scratch::new("recover");
        let plugins = s.0.join("plugins");
        let a = app();
        install(
            &plugins,
            &a,
            &package(&s.0, "OpenTools.bundle", "0.4.0", "old"),
            "0.4.0",
        )
        .unwrap();
        // Simulate a crash right after the old copy was moved aside.
        let backup = plugins.join(".open3d-opentools").join("backup");
        fs::create_dir_all(&backup).unwrap();
        fs::rename(plugins.join("OpenTools.bundle"), backup.join("OpenTools.bundle")).unwrap();
        recover(&plugins, &a).unwrap();
        assert_eq!(installed_version(&plugins, &a).as_deref(), Some("0.4.0"));
        assert_eq!(names(&plugins), vec!["OpenTools.bundle"]);
    }

    #[test]
    fn uninstall_removes_only_the_bundle() {
        let s = Scratch::new("uninstall");
        let plugins = s.0.join("plugins");
        let a = app();
        install(
            &plugins,
            &a,
            &package(&s.0, "OpenTools.bundle", "0.5.0", "x"),
            "0.5.0",
        )
        .unwrap();
        fs::create_dir_all(plugins.join("OpenTools-dev.bundle")).unwrap();
        fs::create_dir_all(plugins.join("VRay3dsMax2027")).unwrap();
        assert_eq!(other_copies(&plugins, &a), vec!["OpenTools-dev.bundle"]);
        uninstall(&plugins, &a).unwrap();
        assert_eq!(installed_version(&plugins, &a), None);
        assert_eq!(names(&plugins), vec!["OpenTools-dev.bundle", "VRay3dsMax2027"]);
        uninstall(&plugins, &a).unwrap(); // nothing to do is fine
    }
}
