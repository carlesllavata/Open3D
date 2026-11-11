// SPDX-License-Identifier: GPL-3.0-or-later
//! Installing the launcher itself, per user and without administrator rights:
//! `%LOCALAPPDATA%\Programs\Open3D Launcher\Open3D Launcher.exe`, Start menu and desktop shortcuts, and an
//! entry in Windows' installed apps whose "Uninstall" runs `--uninstall`. Also replacing the running exe
//! with a newer one (self-update).
//!
//! Windows lets a running exe be renamed, so an update moves the running file aside (`.old`) and puts the
//! new one in its place; the `.old` file is deleted on the next start.

use std::path::{Path, PathBuf};
use std::process::Command;

pub const APP_NAME: &str = "Open3D Launcher";
pub const EXE_NAME: &str = "Open3D Launcher.exe";
const UNINSTALL_KEY: &str = r"HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Open3DLauncher";
const HOMEPAGE: &str = "https://github.com/carlesllavata/Open3D";

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `%LOCALAPPDATA%\Programs\Open3D Launcher`.
pub fn install_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs").join(APP_NAME))
}

pub fn installed_exe() -> Option<PathBuf> {
    install_dir().map(|d| d.join(EXE_NAME))
}

pub fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("Could not find the launcher's own file: {e}"))
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// True when this process is the installed copy.
pub fn is_installed_copy() -> bool {
    match (current_exe(), installed_exe()) {
        (Ok(me), Some(inst)) => inst.exists() && same_path(&me, &inst),
        _ => false,
    }
}

/// True when an installed copy exists (this one or another).
pub fn is_installed() -> bool {
    installed_exe().is_some_and(|p| p.exists())
}

/// Deletes what an earlier update left behind next to the running exe. Right after a self-update the old
/// process may still be closing (its exe is locked until it exits), so this keeps trying for a while on a
/// background thread.
pub fn clean_leftovers() {
    let Ok(me) = current_exe() else { return };
    let files = [sibling(&me, "old"), sibling(&me, "download")];
    if !files.iter().any(|f| f.exists()) {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..40 {
            let mut left = false;
            for f in &files {
                if f.exists() && std::fs::remove_file(f).is_err() {
                    left = true;
                }
            }
            if !left {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

fn sibling(exe: &Path, ext: &str) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

/// Removes the "downloaded from the internet" mark so Windows doesn't warn again for the installed copy.
fn unblock(path: &Path) {
    let mut ads = path.as_os_str().to_owned();
    ads.push(":Zone.Identifier");
    let _ = std::fs::remove_file(PathBuf::from(ads));
}

/// Puts `new_exe` in place of `target`, moving a (possibly running) old copy aside first.
fn replace_file(new_exe: &Path, target: &Path, copy: bool) -> Result<(), String> {
    let old = sibling(target, "old");
    let _ = std::fs::remove_file(&old);
    let had_old = target.exists();
    if had_old {
        std::fs::rename(target, &old).map_err(|e| format!("Could not move the old launcher aside: {e}"))?;
    }
    let placed = if copy {
        std::fs::copy(new_exe, target).map(|_| ())
    } else {
        std::fs::rename(new_exe, target)
    };
    if let Err(e) = placed {
        if had_old {
            let _ = std::fs::rename(&old, target);
        }
        return Err(format!("Could not put the launcher in place: {e}"));
    }
    unblock(target);
    Ok(())
}

/// Copies this exe into the install folder, creates the shortcuts and the installed-apps entry.
/// Returns the installed exe.
pub fn install_self(desktop_shortcut: bool) -> Result<PathBuf, String> {
    let me = current_exe()?;
    let dir = install_dir().ok_or("LOCALAPPDATA is not set")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    let target = dir.join(EXE_NAME);
    if !same_path(&me, &target) {
        replace_file(&me, &target, true)?;
    }
    let size_kb = std::fs::metadata(&target).map(|m| m.len() / 1024).unwrap_or(0);
    let script = format!(
        r#"$ErrorActionPreference = 'Stop'
$exe = {exe}; $dir = {dir}; $name = {name}
$shell = New-Object -ComObject WScript.Shell
$folders = @([Environment]::GetFolderPath('Programs'))
if ({desktop}) {{ $folders += [Environment]::GetFolderPath('Desktop') }}
foreach ($f in $folders) {{
  $l = $shell.CreateShortcut((Join-Path $f "$name.lnk"))
  $l.TargetPath = $exe; $l.WorkingDirectory = $dir; $l.IconLocation = "$exe,0"
  $l.Description = 'Install and update the Open3D apps for 3ds Max'
  $l.Save()
}}
$k = {key}
New-Item -Path $k -Force | Out-Null
Set-ItemProperty -Path $k -Name DisplayName -Value $name
Set-ItemProperty -Path $k -Name DisplayVersion -Value {version}
Set-ItemProperty -Path $k -Name Publisher -Value 'Open3D'
Set-ItemProperty -Path $k -Name DisplayIcon -Value "$exe,0"
Set-ItemProperty -Path $k -Name InstallLocation -Value $dir
Set-ItemProperty -Path $k -Name URLInfoAbout -Value {home}
Set-ItemProperty -Path $k -Name UninstallString -Value ('"' + $exe + '" --uninstall')
New-ItemProperty -Path $k -Name NoModify -Value 1 -PropertyType DWord -Force | Out-Null
New-ItemProperty -Path $k -Name NoRepair -Value 1 -PropertyType DWord -Force | Out-Null
New-ItemProperty -Path $k -Name EstimatedSize -Value {size_kb} -PropertyType DWord -Force | Out-Null
"#,
        exe = ps_str(&target.to_string_lossy()),
        dir = ps_str(&dir.to_string_lossy()),
        name = ps_str(APP_NAME),
        desktop = if desktop_shortcut { "$true" } else { "$false" },
        key = ps_str(UNINSTALL_KEY),
        version = ps_str(env!("CARGO_PKG_VERSION")),
        home = ps_str(HOMEPAGE),
    );
    run_powershell(&script).map_err(|e| format!("The launcher was copied, but its shortcuts failed: {e}"))?;
    Ok(target)
}

/// Updates the version shown in Windows' installed apps (after a self-update).
fn set_registered_version(version: &str) {
    let script = format!(
        "if (Test-Path {key}) {{ Set-ItemProperty -Path {key} -Name DisplayVersion -Value {v} }}",
        key = ps_str(UNINSTALL_KEY),
        v = ps_str(version)
    );
    let _ = run_powershell(&script);
}

/// Removes the shortcuts and the installed-apps entry, then deletes the install folder once this process
/// has exited. The apps installed with the launcher are not touched.
pub fn uninstall_self() -> Result<(), String> {
    let dir = install_dir().ok_or("LOCALAPPDATA is not set")?;
    let script = format!(
        r#"$name = {name}
foreach ($f in @([Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('Desktop'))) {{
  $p = Join-Path $f "$name.lnk"
  if (Test-Path -LiteralPath $p) {{ Remove-Item -LiteralPath $p -Force }}
}}
if (Test-Path {key}) {{ Remove-Item -Path {key} -Recurse -Force }}
"#,
        name = ps_str(APP_NAME),
        key = ps_str(UNINSTALL_KEY),
    );
    run_powershell(&script)?;
    if dir.exists() {
        // The running exe can't delete its own folder: a hidden cmd keeps trying once a second, for up to a
        // minute, until this window has closed and the folder is gone.
        let mut cmd = Command::new("cmd.exe");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let d = dir.display();
            // raw_arg: cmd.exe doesn't understand the \" escaping Rust would add around the path.
            cmd.raw_arg(format!(
                "/C for /l %i in (1,1,60) do @(if exist \"{d}\" (rmdir /s /q \"{d}\" 2>nul & ping 127.0.0.1 -n 2 >nul))"
            ));
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd.spawn()
            .map_err(|e| format!("Could not schedule removing {}: {e}", dir.display()))?;
    }
    Ok(())
}

/// Replaces the running exe with `downloaded` (already checked) and records the new version.
pub fn replace_running(downloaded: &Path, new_version: &str) -> Result<PathBuf, String> {
    let me = current_exe()?;
    replace_file(downloaded, &me, false)?;
    if is_installed_copy() {
        set_registered_version(new_version);
    }
    Ok(me)
}

/// Where a self-update downloads to: next to the running exe, so the final move is a rename.
pub fn download_path() -> Result<PathBuf, String> {
    Ok(sibling(&current_exe()?, "download"))
}

/// Starts `exe` and returns at once. `same_args` passes on this process's own options (after a
/// self-update, so the new version opens the same way).
pub fn launch(exe: &Path, same_args: bool) -> Result<(), String> {
    let args: Vec<String> = if same_args {
        std::env::args().skip(1).collect()
    } else {
        Vec::new()
    };
    Command::new(exe)
        .args(args)
        .current_dir(exe.parent().unwrap_or_else(|| Path::new(".")))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not start {}: {e}", exe.display()))
}

/// A PowerShell single-quoted string literal.
fn ps_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn run_powershell(script: &str) -> Result<(), String> {
    let mut cmd = Command::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-EncodedCommand",
    ])
    .arg(encode_command(script));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("Could not run PowerShell: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// PowerShell's `-EncodedCommand`: base64 of the UTF-16LE script (no quoting problems).
fn encode_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    base64(&bytes)
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn powershell_strings_escape_quotes() {
        assert_eq!(ps_str("O'Neil"), "'O''Neil'");
    }

    #[test]
    fn replace_file_swaps_and_keeps_the_old_copy_aside() {
        let dir = std::env::temp_dir().join(format!("open3d-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("app.exe");
        let new = dir.join("app.exe.download");
        std::fs::write(&target, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        replace_file(&new, &target, false).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(std::fs::read(sibling(&target, "old")).unwrap(), b"old");
        assert!(!new.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
