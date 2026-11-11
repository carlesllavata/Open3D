# Launcher installation

The MSI installs Open3D Launcher for the current user only, with no administrator rights, by
default into `%LOCALAPPDATA%\Programs\Open3D\Launcher`. It records the folder and the
desktop-shortcut choice under `HKCU\Software\Open3D\Open3D Launcher`, so an update (the
launcher's self-update runs the new MSI with `/passive`) lands in the same folder and keeps the
choice; an explicit `INSTALLFOLDER` on the command line wins. Staying per-user is what lets the
launcher update itself without elevation.

The portable zip needs no installer: `portable.txt` next to `open3d-launcher.exe` keeps the
settings and download cache in `Open3DLauncherData\` beside it.

The apps themselves are 3ds Max plug-in bundles: the launcher unpacks them into
`%APPDATA%\Autodesk\ApplicationPlugins\<App>.bundle`, where every 3ds Max version loads them.

`./packaging/windows/test-install-policy.ps1 -OutputDirectory ./target/install-policy-tests`
builds non-executable MSIs for x64, x86 and arm64 and checks their compiled paths, identity and
registration (`check-install-policy.ps1`). Never install them. `package.ps1` runs the same check
on the real MSI.
