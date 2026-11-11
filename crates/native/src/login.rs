// SPDX-License-Identifier: GPL-3.0-or-later
//! "Start Open3D Launcher when I log in".
//!
//! - macOS: a LaunchAgent in `~/Library/LaunchAgents`.
//! - Windows: a value under `HKCU\…\CurrentVersion\Run`.
//! - Linux and FreeBSD: an XDG autostart entry in `~/.config/autostart` (the host's, from the
//!   Flatpak, starting the app through `flatpak run`).
//!
//! The launcher starts hidden in the tray when it's started this way (`--hidden`).

use std::path::PathBuf;

/// The command-line flag a login start passes.
pub const HIDDEN_FLAG: &str = "--hidden";

/// The program to start at login: the AppImage when running from one, else this executable.
fn program() -> Result<PathBuf, String> {
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        return Ok(PathBuf::from(appimage));
    }
    std::env::current_exe().map_err(|e| format!("couldn't find this program ({e})"))
}

pub fn is_enabled() -> bool {
    #[cfg(windows)]
    return windows_run().and_then(|k| k.get_value::<String, _>("Open3D Launcher").ok()).is_some();
}

pub fn set_enabled(on: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::HKEY_CURRENT_USER;
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
            .map_err(|e| format!("couldn't change the startup list ({e})"))?;
        if on {
            let value = format!("\"{}\" {HIDDEN_FLAG}", program()?.display());
            key.set_value("Open3D Launcher", &value).map_err(|e| format!("couldn't change the startup list ({e})"))
        } else {
            match key.delete_value("Open3D Launcher") {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("couldn't change the startup list ({e})")),
            }
        }
    }
}

#[cfg(windows)]
fn windows_run() -> Option<winreg::RegKey> {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run").ok()
}
