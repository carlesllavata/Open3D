// SPDX-License-Identifier: GPL-3.0-or-later
//! The Windows Installer's own record of an MSI install (`Open3D`), for the launcher's self-update.

use crate::{App, version};

/// Who our Windows Installer packages name as their publisher.
const PUBLISHER: &str = "Open3D";

/// Whether an uninstall entry's `DisplayName` and `Publisher` are `app`'s, under any name its
/// installers have registered.
fn is_installer_entry(app: &App, display_name: &str, publisher: &str) -> bool {
    publisher == PUBLISHER && app.names().any(|n| n == display_name)
}

#[cfg(windows)]
pub mod windows {

    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};

    use super::{App, is_installer_entry, version};

    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";

    /// An MSI install found in the registry.
    pub struct MsiInstall {
        pub product_code: String,
        pub version: Option<crate::version::Version>,
    }

    /// Every Windows Installer entry for `app`, newest first.
    pub fn msi_installs(app: &App) -> Vec<MsiInstall> {
        let mut out = Vec::new();
        for (hive, flags) in [(HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY), (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY), (HKEY_CURRENT_USER, 0)] {
            let Ok(root) = RegKey::predef(hive).open_subkey_with_flags(UNINSTALL, KEY_READ | flags) else { continue };
            for code in root.enum_keys().flatten() {
                let Ok(key) = root.open_subkey_with_flags(&code, KEY_READ | flags) else { continue };
                let name: String = key.get_value("DisplayName").unwrap_or_default();
                let publisher: String = key.get_value("Publisher").unwrap_or_default();
                if !is_installer_entry(app, &name, &publisher) || out.iter().any(|m: &MsiInstall| m.product_code == code) {
                    continue;
                }
                let version = key.get_value::<String, _>("DisplayVersion").ok().and_then(|v| version::parse(&v));
                out.push(MsiInstall { product_code: code, version });
            }
        }
        out.sort_by(|a, b| match (&b.version, &a.version) {
            (Some(x), Some(y)) => version::compare(x, y),
            (Some(_), None) => std::cmp::Ordering::Greater,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (None, None) => std::cmp::Ordering::Equal,
        });
        out
    }
}
