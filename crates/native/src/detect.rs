// SPDX-License-Identifier: GPL-3.0-or-later
//! Finding Open3D apps that are already installed, however they got there.
//!
//! - macOS: `<App>.app` in `/Applications` or `~/Applications`, read from its `Info.plist`.
//! - Windows: the Windows Installer's uninstall entries (`Open3D`), and portable
//!   copies the launcher unpacked.
//! - Linux: AppImages in `~/Applications` (where the launcher and AppImageLauncher put them),
//!   `~/AppImages` (Gear Lever) and `~/.local/bin`, Debian/RPM packages, Flatpaks, and tarball
//!   installs.
//! - FreeBSD: tarball installs and `/usr/local/bin/<app>`.

use std::path::{Path, PathBuf};

use open3d_launcher_core::{InstallSource, Installed, SuiteApp, version};

use crate::manifest::Manifest;

/// What's installed of `app`, best source first.
pub fn detect(app: &SuiteApp, manifest: &Manifest) -> Option<Installed> {
    if let Some(found) = from_manifest(app, manifest) {
        return Some(found);
    }
    // Open3D apps are 3ds Max bundles: one in ApplicationPlugins is found whoever put it there.
    if let Some(found) = crate::max_bundle::detect(app) {
        return Some(found);
    }
    #[cfg(windows)]
    return windows::detect(app);
    #[allow(unreachable_code)]
    None
}

/// Other copies of `app` beside the one [`detect`] reports, by folder name: development or renamed
/// bundles (`OpenTools-dev.bundle`) in 3ds Max's plug-in folder, of which Max loads only one.
/// Reported whether or not the app itself is installed, since installing it would add another.
pub fn other_copies(app: &SuiteApp) -> Vec<String> {
    crate::max_bundle::other_copies(app)
}

fn from_manifest(app: &SuiteApp, manifest: &Manifest) -> Option<Installed> {
    let rec = manifest.apps.get(app.id)?;
    if !rec.path.exists() {
        return None;
    }
    // A 3ds Max bundle's own PackageContents.xml is the truth (it may have been replaced by hand).
    if rec.kind == open3d_launcher_core::PackageKind::MaxBundle {
        let mut found = crate::max_bundle::detect(app)?;
        found.source = InstallSource::Launcher;
        return Some(found);
    }
    // On macOS the bundle itself is the truth (the app may have updated itself).
    Some(Installed {
        version: version::parse(&rec.version),
        source: InstallSource::Launcher,
        location: rec.path.display().to_string(),
        package: None,
        externally_managed: false,
        origin: None,
        installation: None,
    })
}

/// Who our Windows Installer packages name as their publisher. A community app's names its own
/// ([`SuiteApp::msi_publisher`]).
const PUBLISHER: &str = "Open3D";

/// Whether an uninstall entry's `DisplayName` and `Publisher` are `app`'s, under any name its
/// installers have registered.
fn is_installer_entry(app: &SuiteApp, display_name: &str, publisher: &str) -> bool {
    publisher == app.msi_publisher.unwrap_or(PUBLISHER) && app.names().any(|n| n == display_name)
}

/// Where a Windows install of `app` may keep its exe, in the order to try: the entry's
/// `InstallLocation`, then `<App>` under each install root (the `Open3D` suite folder first, then the
/// root itself, for Program Files and the per-user Programs folder) for each name the app has had,
/// and in each folder `<stem>.exe` for each stem, current first. Doesn't touch the disk.
fn exe_candidates(app: &SuiteApp, location: Option<&Path>, program_files: &[PathBuf]) -> Vec<PathBuf> {
    let in_program_files = program_files.iter().flat_map(|root| app.names().map(move |n| root.join(n)));
    let dirs = location.map(Path::to_path_buf).into_iter().chain(in_program_files);
    dirs.flat_map(|d| app.stems().map(move |s| d.join(format!("{s}.exe")))).collect()
}

/// The folder under `Open3D` that the MSI installs to when it is not the display name: the launcher
/// installs to `Open3D\Launcher`, the apps to `Open3D\<App>`.
fn suite_folders(app: &SuiteApp, roots: &[PathBuf]) -> Vec<PathBuf> {
    if app.id != open3d_launcher_core::catalog::LAUNCHER_ID {
        return Vec::new();
    }
    roots.iter().map(|root| root.join("Open3D").join("Launcher")).collect()
}

/// The registry keys an MSI records its install folder in (value `InstallDir`). The launcher's package
/// keeps its own key, under the publisher name.
fn install_dir_keys(app: &SuiteApp) -> Vec<String> {
    let mut keys = vec![format!(r"SOFTWARE\{}", app.name)];
    if app.id == open3d_launcher_core::catalog::LAUNCHER_ID {
        keys.push(format!(r"SOFTWARE\{PUBLISHER}\{}", app.name));
    }
    keys
}

/// Newest version first; an entry with no version comes after every entry with one.
#[cfg(any(windows, test))]
fn newest_first(a: Option<&version::Version>, b: Option<&version::Version>) -> std::cmp::Ordering {
    match (b, a) {
        (Some(x), Some(y)) => version::compare(x, y),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

#[cfg(windows)]
pub mod windows {
    use std::path::PathBuf;

    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
    use winreg::{HKEY, RegKey};

    use super::{InstallSource, Installed, SuiteApp, exe_candidates, install_dir_keys, is_installer_entry, suite_folders, version};

    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";

    /// Reads the ProductVersion string from a program's VERSIONINFO, in the first language that
    /// has one, without running the program or any other process.
    pub fn executable_version(path: &std::path::Path) -> Option<open3d_launcher_core::version::Version> {
        let path = dunce::canonicalize(path).ok()?;
        let info = winsafe::HVERSIONINFO::GetFileVersionInfo(path.to_str()?).ok()?;
        let listed = info.langs_and_cps().map(<[_]>::to_vec).unwrap_or_default();
        let product = string_tables(&listed).into_iter().find_map(|table| info.str_val(table, "ProductVersion").ok())?;
        version::parse(product.trim())
    }

    /// The string tables to look in: the languages the file lists, then U.S. English in UTF-16,
    /// Windows-1252 and no code page, as .NET's `FileVersionInfo` also tries when a file lists none
    /// or its listed one has no strings.
    pub(crate) fn string_tables(listed: &[(winsafe::LANGID, winsafe::co::CP)]) -> Vec<(winsafe::LANGID, winsafe::co::CP)> {
        use winsafe::{LANGID, co};
        let us = LANGID::new(co::LANG::ENGLISH, co::SUBLANG::ENGLISH_US);
        listed.iter().copied().chain([(us, co::CP::UTF16), (us, co::CP::WINDOWS_1252), (us, co::CP::ACP)]).collect()
    }

    /// An MSI install found in the registry.
    pub struct MsiInstall {
        pub product_code: String,
        pub version: Option<open3d_launcher_core::version::Version>,
        pub exe: Option<PathBuf>,
        /// The context of the uninstall record that named it: the machine's records are per-machine,
        /// the current account's own are per-user.
        pub scope: crate::install::MsiScope,
    }

    /// Where the uninstall records of an MSI are kept: the machine's (both registry views) and the account's own.
    const UNINSTALL_ROOTS: [(HKEY, u32); 3] = [(HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY), (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY), (HKEY_CURRENT_USER, 0)];

    fn record_context(hive: HKEY) -> crate::install::MsiScope {
        if hive == HKEY_CURRENT_USER { crate::install::MsiScope::PerUser } else { crate::install::MsiScope::PerMachine }
    }

    /// The context every uninstall record of `product_code` places it in, when Windows Installer cannot be
    /// asked. All records are read, not the deduplicated list, so a copy registered both per-user and
    /// per-machine gives no answer and the update stops.
    pub fn record_scope(app: &SuiteApp, product_code: &str) -> Option<crate::install::MsiScope> {
        let scopes = UNINSTALL_ROOTS.into_iter().filter_map(|(hive, flags)| {
            let root = RegKey::predef(hive).open_subkey_with_flags(UNINSTALL, KEY_READ | flags).ok()?;
            let key = root.open_subkey_with_flags(product_code, KEY_READ | flags).ok()?;
            let name: String = key.get_value("DisplayName").unwrap_or_default();
            let publisher: String = key.get_value("Publisher").unwrap_or_default();
            is_installer_entry(app, &name, &publisher).then(|| record_context(hive))
        });
        crate::msi_context::single_scope(scopes)
    }

    /// How an exe path is stored in `Installed::location`. One function builds it for `detect` and for
    /// the comparison below, so the two can never disagree on spelling.
    fn location_text(exe: &std::path::Path) -> String {
        exe.display().to_string()
    }

    /// Whether `location` is the exe the record `product_code` resolves in its own scope. False for
    /// a path that only the launch fallback (`exe_in_any_scope`) found, so an update never takes its
    /// folder from another scope.
    pub fn location_is_in_record_scope(app: &SuiteApp, product_code: &str, location: &str) -> bool {
        msi_installs(app).into_iter().find(|m| m.product_code == product_code).and_then(|m| m.exe).is_some_and(|exe| location_text(&exe) == location)
    }

    /// Every Windows Installer entry for `app`, newest first.
    pub fn msi_installs(app: &SuiteApp) -> Vec<MsiInstall> {
        let mut out = Vec::new();
        for (hive, flags) in UNINSTALL_ROOTS {
            let Ok(root) = RegKey::predef(hive).open_subkey_with_flags(UNINSTALL, KEY_READ | flags) else { continue };
            for code in root.enum_keys().flatten() {
                // App-authored uninstall rows are not MSI product identities.
                if !crate::msi_context::valid_product_code(&code) {
                    continue;
                }
                let Ok(key) = root.open_subkey_with_flags(&code, KEY_READ | flags) else { continue };
                let name: String = key.get_value("DisplayName").unwrap_or_default();
                let publisher: String = key.get_value("Publisher").unwrap_or_default();
                if !is_installer_entry(app, &name, &publisher) || out.iter().any(|m: &MsiInstall| m.product_code == code) {
                    continue;
                }
                let version = key.get_value::<String, _>("DisplayVersion").ok().and_then(|v| version::parse(&v));
                let location: String = key.get_value("InstallLocation").unwrap_or_default();
                let exe =
                    exe_path(app, (!location.is_empty()).then(|| PathBuf::from(location)).or_else(|| install_dir(app, hive, flags)), hive, flags);
                out.push(MsiInstall { product_code: code, version, exe, scope: record_context(hive) });
            }
        }
        out.sort_by(|a, b| super::newest_first(a.version.as_ref(), b.version.as_ref()));
        out
    }

    /// The folder the app's MSI remembers for its next upgrade, read from the same registry view as the
    /// uninstall record that named the install. A per-machine record (HKLM) therefore reads HKLM, and a
    /// per-user record (HKCU, dual-scope packages) reads HKCU, so one scope never resolves to the other's folder.
    fn install_dir(app: &SuiteApp, hive: HKEY, flags: u32) -> Option<PathBuf> {
        install_dir_keys(app).into_iter().find_map(|path| {
            let key = RegKey::predef(hive).open_subkey_with_flags(path, KEY_READ | flags).ok()?;
            let dir = key.get_value::<String, _>("InstallDir").ok()?;
            (!dir.is_empty()).then(|| PathBuf::from(dir))
        })
    }

    /// The app's exe for one uninstall record, found only in that record's own scope: the recorded
    /// folder first, then `App Paths` in the record's hive and registry view, then the usual folders
    /// for that scope (`Open3D` suite folder first, then the bare root, for older installs). A
    /// per-machine entry therefore never stands in for a per-user record, or the other way round.
    fn exe_path(app: &SuiteApp, location: Option<PathBuf>, hive: HKEY, flags: u32) -> Option<PathBuf> {
        if let Some(found) = exe_candidates(app, location.as_deref(), &[]).into_iter().find(|p| p.exists()) {
            return Some(found);
        }
        for stem in app.stems() {
            let app_paths = format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{stem}.exe");
            if let Ok(key) = RegKey::predef(hive).open_subkey_with_flags(&app_paths, KEY_READ | flags)
                && let Ok(path) = key.get_value::<String, _>("")
            {
                let p = PathBuf::from(path.trim_matches('"'));
                if p.exists() {
                    return Some(p);
                }
            }
        }
        let roots: Vec<PathBuf> = if hive == HKEY_CURRENT_USER {
            // Per-user installs (dual-scope packages) live in the user's own Programs folder.
            std::env::var_os("LOCALAPPDATA").map(|local| PathBuf::from(local).join("Programs")).into_iter().collect()
        } else {
            let mut roots: Vec<PathBuf> = Vec::new();
            for var in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
                if let Some(pf) = std::env::var_os(var).map(PathBuf::from)
                    && !roots.contains(&pf)
                {
                    roots.push(pf);
                }
            }
            roots
        };
        exe_in_roots(app, roots)
    }

    /// Where the exe may be under the given install roots, best first: the launcher's own folder, then
    /// the `Open3D` suite folder of each root, then the bare roots (older installs). Not checked on
    /// disk.
    fn candidates_in_roots(app: &SuiteApp, roots: &[PathBuf]) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = suite_folders(app, roots).iter().flat_map(|dir| exe_candidates(app, Some(dir), &[])).collect();
        let mut dirs: Vec<PathBuf> = roots.iter().map(|root| root.join("Open3D")).collect();
        dirs.extend(roots.iter().cloned());
        out.extend(exe_candidates(app, None, &dirs));
        out
    }

    /// The first exe under the given install roots.
    pub(super) fn exe_in_roots(app: &SuiteApp, roots: Vec<PathBuf>) -> Option<PathBuf> {
        candidates_in_roots(app, &roots).into_iter().find(|p| p.exists())
    }

    /// Every exe under the given install roots, in the order `exe_in_roots` prefers them.
    pub(super) fn exe_all_in_roots(app: &SuiteApp, roots: Vec<PathBuf>) -> Vec<PathBuf> {
        candidates_in_roots(app, &roots).into_iter().filter(|p| p.exists()).collect()
    }

    /// The app's exe directly in `folder`, under any name it has shipped as.
    pub(super) fn exe_in_folder(app: &SuiteApp, folder: &std::path::Path) -> Option<PathBuf> {
        exe_candidates(app, Some(folder), &[]).into_iter().find(|p| p.exists())
    }

    /// Last resort for opening an app whose uninstall record names no folder and whose exe is not in
    /// the record's own scope. The package records its folder (`InstallDir`) in the hive of the account
    /// that installed it, which need not be the hive of its uninstall record: a per-user install can
    /// leave a per-machine record. So the folder any hive or view recorded is tried first (this finds
    /// a custom folder), then the usual folders of every scope. This only decides which exe "Open"
    /// runs and where "Show in folder" goes. An update never takes its folder from here
    /// (`location_is_in_record_scope`), and the record's scope is unchanged. Every candidate is
    /// collected and the one whose own version is the record's is chosen, so a leftover older copy in
    /// another scope is not opened under the newer record's version (`pick_candidate`).
    fn exe_in_any_scope(app: &SuiteApp, version: Option<&version::Version>) -> Option<PathBuf> {
        let mut found: Vec<PathBuf> =
            UNINSTALL_ROOTS.into_iter().filter_map(|(hive, flags)| exe_in_folder(app, &install_dir(app, hive, flags)?)).collect();
        let mut roots: Vec<PathBuf> = std::env::var_os("LOCALAPPDATA").map(|local| PathBuf::from(local).join("Programs")).into_iter().collect();
        for var in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(pf) = std::env::var_os(var).map(PathBuf::from)
                && !roots.contains(&pf)
            {
                roots.push(pf);
            }
        }
        found.extend(exe_all_in_roots(app, roots));
        pick_candidate(found, version, executable_version)
    }

    /// The first candidate whose version is `wanted`; with no match (or no version to match, or exes
    /// without version info) the first candidate, in the order they were found.
    pub(super) fn pick_candidate(
        candidates: Vec<PathBuf>,
        wanted: Option<&version::Version>,
        version_of: impl Fn(&std::path::Path) -> Option<version::Version>,
    ) -> Option<PathBuf> {
        let matching = wanted.and_then(|want| candidates.iter().find(|c| version_of(c).as_ref() == Some(want)));
        matching.or_else(|| candidates.first()).cloned()
    }

    pub fn detect(app: &SuiteApp) -> Option<Installed> {
        let m = msi_installs(app).into_iter().next()?;
        let location = m.exe.or_else(|| exe_in_any_scope(app, m.version.as_ref())).map_or_else(|| app.name.to_string(), |p| location_text(&p));
        Some(Installed {
            version: m.version,
            source: InstallSource::Msi,
            location,
            package: Some(m.product_code),
            externally_managed: false,
            origin: None,
            installation: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use open3d_launcher_core::SUITE;

    use super::*;

    #[test]
    fn msi_entries_sort_newest_first_and_unversioned_last() {
        use std::cmp::Ordering::{Equal, Greater, Less};
        let (old, new) = (version::parse("0.4.0"), version::parse("0.5.0"));
        // Each arm by hand: the sort below never asks about None against Some in that order.
        assert_eq!(newest_first(new.as_ref(), old.as_ref()), Less);
        assert_eq!(newest_first(old.as_ref(), new.as_ref()), Greater);
        assert_eq!(newest_first(new.as_ref(), None), Less);
        assert_eq!(newest_first(None, new.as_ref()), Greater);
        assert_eq!(newest_first(None, None), Equal);
        let mut versions = vec![None, old.clone(), new.clone(), None];
        versions.sort_by(|a, b| newest_first(a.as_ref(), b.as_ref()));
        assert_eq!(versions, vec![new, old, None, None]);
    }

    /// A file without VERSIONINFO, or one that isn't there, has no version.
    #[cfg(windows)]
    #[test]
    fn a_file_without_version_info_has_no_version() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("phototool.exe");
        std::fs::write(&text, "not a program").unwrap();
        assert_eq!(windows::executable_version(&text), None);
        assert_eq!(windows::executable_version(&dir.path().join("missing.exe")), None);
    }

    /// Without a listed language, the version is still looked for in the string tables .NET's
    /// `FileVersionInfo` falls back to (040904B0, 040904E4, 04090000), after any listed one.
    #[cfg(windows)]
    #[test]
    fn the_version_is_looked_for_in_the_usual_string_tables_too() {
        use winsafe::{LANGID, co};
        let key = |(lang, cp): (LANGID, co::CP)| format!("{:04x}{:04x}", u16::from(lang), u16::from(cp));
        let keys = |listed: &[(LANGID, co::CP)]| windows::string_tables(listed).into_iter().map(key).collect::<Vec<_>>();
        assert_eq!(keys(&[]), ["040904b0", "040904e4", "04090000"]);
        let german = (LANGID::new(co::LANG::GERMAN, co::SUBLANG::GERMAN), co::CP::UTF16);
        assert_eq!(keys(&[german]), ["040704b0", "040904b0", "040904e4", "04090000"]);
    }

    /// A program whose VERSIONINFO has a ProductVersion in the U.S. English UTF-16 table but no
    /// `\VarFileInfo\Translation` entry still has a version.
    #[cfg(windows)]
    #[test]
    fn a_version_without_a_translation_entry_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("phototool.exe");
        std::fs::write(&exe, resource_only_program("1.2.3")).unwrap();
        assert_eq!(windows::executable_version(&exe), version::parse("1.2.3"));
    }

    /// A VERSIONINFO block node: length, value length, type, the key, then the value and the
    /// children, each aligned to 4 bytes.
    #[cfg(windows)]
    fn version_node(key: &str, value: &[u8], value_length: u16, text: bool, children: &[Vec<u8>]) -> Vec<u8> {
        let align = |bytes: &mut Vec<u8>| bytes.resize(bytes.len().next_multiple_of(4), 0);
        let mut node = vec![0, 0];
        node.extend(value_length.to_le_bytes());
        node.extend(u16::from(text).to_le_bytes());
        node.extend(key.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
        align(&mut node);
        node.extend(value);
        for child in children {
            align(&mut node);
            node.extend(child);
        }
        let length = u16::try_from(node.len()).unwrap();
        node[..2].copy_from_slice(&length.to_le_bytes());
        node
    }

    /// A 64-bit program file holding only a version resource (`ProductVersion` = `product`, in
    /// the 040904B0 string table) and no `\VarFileInfo\Translation` entry.
    #[cfg(windows)]
    fn resource_only_program(product: &str) -> Vec<u8> {
        let put = |bytes: &mut Vec<u8>, at: usize, value: &[u8]| bytes[at..at + value.len()].copy_from_slice(value);
        let text: Vec<u8> = product.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
        let string = version_node("ProductVersion", &text, u16::try_from(product.len() + 1).unwrap(), true, &[]);
        let table = version_node("040904b0", &[], 0, true, &[string]);
        let strings = version_node("StringFileInfo", &[], 0, true, &[table]);
        let mut fixed = vec![0u8; 52];
        put(&mut fixed, 0, &0xFEEF_04BDu32.to_le_bytes());
        put(&mut fixed, 4, &0x0001_0000u32.to_le_bytes());
        let info = version_node("VS_VERSION_INFO", &fixed, 52, false, &[strings]);

        // The resource section: type 16 (RT_VERSION) > id 1 > language 0x0409 > the data.
        const RVA: u32 = 0x1000;
        let mut rsrc = vec![0u8; 0x58];
        for (dir, id, next) in [(0x00, 16u32, 0x8000_0018u32), (0x18, 1, 0x8000_0030), (0x30, 0x0409, 0x48)] {
            put(&mut rsrc, dir + 14, &1u16.to_le_bytes());
            put(&mut rsrc, dir + 16, &id.to_le_bytes());
            put(&mut rsrc, dir + 20, &next.to_le_bytes());
        }
        put(&mut rsrc, 0x48, &(RVA + 0x58).to_le_bytes());
        put(&mut rsrc, 0x4C, &u32::try_from(info.len()).unwrap().to_le_bytes());
        rsrc.extend(&info);
        let raw = u32::try_from(rsrc.len().next_multiple_of(0x200)).unwrap();
        let rsrc_len = u32::try_from(rsrc.len()).unwrap();

        let mut file = vec![0u8; 0x200];
        put(&mut file, 0, b"MZ");
        put(&mut file, 0x3C, &0x40u32.to_le_bytes());
        put(&mut file, 0x40, b"PE\0\0");
        // COFF header: x64, one section, a 240-byte optional header, an executable DLL image.
        put(&mut file, 0x44, &0x8664u16.to_le_bytes());
        put(&mut file, 0x46, &1u16.to_le_bytes());
        put(&mut file, 0x54, &240u16.to_le_bytes());
        put(&mut file, 0x56, &0x2022u16.to_le_bytes());
        // PE32+ optional header.
        let opt = 0x58;
        put(&mut file, opt, &0x20Bu16.to_le_bytes());
        put(&mut file, opt + 8, &raw.to_le_bytes());
        put(&mut file, opt + 24, &0x1_4000_0000u64.to_le_bytes());
        put(&mut file, opt + 32, &0x1000u32.to_le_bytes());
        put(&mut file, opt + 36, &0x200u32.to_le_bytes());
        put(&mut file, opt + 40, &6u16.to_le_bytes());
        put(&mut file, opt + 48, &6u16.to_le_bytes());
        put(&mut file, opt + 56, &(RVA + rsrc_len.next_multiple_of(0x1000)).to_le_bytes());
        put(&mut file, opt + 60, &0x200u32.to_le_bytes());
        put(&mut file, opt + 68, &2u16.to_le_bytes());
        put(&mut file, opt + 72, &0x10_0000u64.to_le_bytes());
        put(&mut file, opt + 80, &0x1000u64.to_le_bytes());
        put(&mut file, opt + 88, &0x10_0000u64.to_le_bytes());
        put(&mut file, opt + 96, &0x1000u64.to_le_bytes());
        put(&mut file, opt + 108, &16u32.to_le_bytes());
        put(&mut file, opt + 112 + 2 * 8, &RVA.to_le_bytes());
        put(&mut file, opt + 112 + 2 * 8 + 4, &rsrc_len.to_le_bytes());
        // The .rsrc section header.
        let section = opt + 240;
        put(&mut file, section, b".rsrc\0\0\0");
        put(&mut file, section + 8, &rsrc_len.to_le_bytes());
        put(&mut file, section + 12, &RVA.to_le_bytes());
        put(&mut file, section + 16, &raw.to_le_bytes());
        put(&mut file, section + 20, &0x200u32.to_le_bytes());
        put(&mut file, section + 36, &0x4000_0040u32.to_le_bytes());
        rsrc.resize(usize::try_from(raw).unwrap(), 0);
        file.extend(rsrc);
        file
    }

    const LEARNING_MACHINES: &str = "Open3D";

    fn roots() -> Vec<PathBuf> {
        vec![PathBuf::from(r"C:\Program Files"), PathBuf::from(r"C:\Program Files (x86)")]
    }

    #[test]
    fn apps_without_former_names_match_and_find_their_exe_as_before() {
        let roots = roots();
        let location = PathBuf::from(r"D:\Apps\Somewhere");
        for a in SUITE.iter().filter(|a| a.former_ids.is_empty() && a.former_names.is_empty()) {
            let publisher = a.msi_publisher.unwrap_or(LEARNING_MACHINES);
            assert!(is_installer_entry(a, a.name, publisher), "{}", a.name);
            assert!(!is_installer_entry(a, a.name, "Some Other Publisher"), "{}", a.name);
            assert!(!is_installer_entry(a, &a.name.to_lowercase(), publisher), "{}", a.name);
            let exe = format!("{}.exe", a.id);
            let in_program_files: Vec<PathBuf> = roots.iter().map(|r| r.join(a.name).join(&exe)).collect();
            assert_eq!(exe_candidates(a, None, &roots), in_program_files);
            let with_location: Vec<PathBuf> = std::iter::once(location.join(&exe)).chain(in_program_files).collect();
            assert_eq!(exe_candidates(a, Some(&location), &roots), with_location);
        }
    }

    /// With two copies in different scopes, the one that is the record's version is chosen, wherever it
    /// was found; with no match, the first found.
    #[cfg(windows)]
    #[test]
    fn the_copy_with_the_records_version_is_the_one_opened() {
        let v = |s: &str| version::parse(s);
        let old = PathBuf::from(r"D:\Old\decktool.exe");
        let new = PathBuf::from(r"C:\Users\a\AppData\Local\Programs\Open3D\DeckTool\decktool.exe");
        let version_of = |p: &std::path::Path| if p == old { v("0.11.1") } else { v("0.12.0") };
        let both = vec![old.clone(), new.clone()];
        assert_eq!(windows::pick_candidate(both.clone(), v("0.12.0").as_ref(), version_of), Some(new.clone()));
        assert_eq!(windows::pick_candidate(both.clone(), v("0.11.1").as_ref(), version_of), Some(old.clone()));
        assert_eq!(windows::pick_candidate(both.clone(), v("0.99.0").as_ref(), version_of), Some(old.clone()));
        assert_eq!(windows::pick_candidate(both, None, version_of), Some(old.clone()));
        assert_eq!(windows::pick_candidate(Vec::new(), v("0.12.0").as_ref(), version_of), None);
    }
}
