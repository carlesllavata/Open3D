// SPDX-License-Identifier: GPL-3.0-or-later
//! Platforms, package formats, and choosing the right release asset.
//!
//! Every Open3D app release names its files `<app>-<version>-<os>-<arch>[-portable].<ext>`, for
//! example `phototool-0.5.0-windows-x64.msi` or `phototool-0.5.0-linux-aarch64.AppImage`.
//!
//! Labels are English. The UI translates [`PackageKind::label`]; OS and architecture names are
//! shown as they are.

use serde::{Deserialize, Serialize};

use crate::release::{Asset, Release};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
    FreeBsd,
}

impl Os {
    pub fn label(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::MacOs => "macOS",
            Self::Linux => "Linux",
            Self::FreeBsd => "FreeBSD",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Arch {
    X64,
    X86,
    Arm64,
    Riscv64,
}

impl Arch {
    pub fn label(self) -> &'static str {
        match self {
            Self::X64 => "x64",
            Self::X86 => "x86",
            Self::Arm64 => "ARM64",
            Self::Riscv64 => "RISC-V",
        }
    }
}

/// A package format a release ships.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PackageKind {
    /// Windows Installer package (per user by default; an existing per-machine copy stays per-machine).
    Msi,
    /// Windows zip with the exe at the top level.
    PortableZip,
    /// macOS disk image with the `.app` bundle.
    Dmg,
    AppImage,
    Deb,
    Rpm,
    /// Arch Linux package (`.pkg.tar.zst`).
    ArchPkg,
    Flatpak,
    /// Linux or FreeBSD tarball (`bin/<app>`, `share/…`).
    TarGz,
    /// Open3D: a 3ds Max plug-in bundle, a zip whose root holds `<App>.bundle`, unpacked into
    /// `ApplicationPlugins` (`<app>-<version>-windows-x64-3dsmax.zip`).
    MaxBundle,
}

impl PackageKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Msi => "Installer (.msi)",
            Self::PortableZip => "Portable (.zip)",
            Self::Dmg => "Disk image (.dmg)",
            Self::AppImage => "AppImage",
            Self::Deb => "Debian package (.deb)",
            Self::Rpm => "RPM package (.rpm)",
            Self::ArchPkg => "Arch package (.pkg.tar.zst)",
            Self::Flatpak => "Flatpak bundle",
            Self::TarGz => "Tarball (.tar.gz)",
            Self::MaxBundle => "3ds Max plug-in (.zip)",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Msi => ".msi",
            Self::PortableZip => "-portable.zip",
            Self::Dmg => ".dmg",
            Self::AppImage => ".AppImage",
            Self::Deb => ".deb",
            Self::Rpm => ".rpm",
            Self::ArchPkg => ".pkg.tar.zst",
            Self::Flatpak => ".flatpak",
            Self::TarGz => ".tar.gz",
            Self::MaxBundle => "-3dsmax.zip",
        }
    }
}

/// Where the launcher is running (or, on the web, the visitor's machine).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
}

impl Platform {
    /// The platform this binary was built for, or `None` for wasm and unsupported targets.
    pub fn current() -> Option<Self> {
        let os = if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(target_os = "linux") {
            Os::Linux
        } else if cfg!(target_os = "freebsd") {
            Os::FreeBsd
        } else {
            return None;
        };
        let arch = if cfg!(target_arch = "x86_64") {
            Arch::X64
        } else if cfg!(target_arch = "x86") {
            Arch::X86
        } else if cfg!(target_arch = "aarch64") {
            Arch::Arm64
        } else if cfg!(target_arch = "riscv64") {
            Arch::Riscv64
        } else {
            return None;
        };
        Some(Self { os, arch })
    }

    pub fn label(self) -> String {
        match self.os {
            Os::MacOs => "macOS".into(),
            os => format!("{} {}", os.label(), self.arch.label()),
        }
    }

    /// Asset platform tokens to try, best first. Windows on ARM and 64-bit Windows can fall back
    /// to emulated builds; macOS prefers universal, then native builds. Apple silicon can fall
    /// back to Intel builds, but Intel Macs cannot run ARM builds.
    fn tokens(self) -> &'static [&'static str] {
        match (self.os, self.arch) {
            (Os::Windows, Arch::X64) => &["windows-x64", "windows-x86"],
            (Os::Windows, Arch::X86) => &["windows-x86"],
            (Os::Windows, Arch::Arm64) => &["windows-arm64", "windows-x64", "windows-x86"],
            (Os::Windows, _) => &[],
            (Os::MacOs, Arch::X64) => &["macos-universal", "macos-x86_64"],
            (Os::MacOs, Arch::Arm64) => &["macos-universal", "macos-aarch64", "macos-arm64", "macos-x86_64"],
            (Os::MacOs, _) => &[],
            (Os::Linux, Arch::X64) => &["linux-x86_64"],
            (Os::Linux, Arch::Arm64) => &["linux-aarch64"],
            (Os::Linux, Arch::Riscv64) => &["linux-riscv64"],
            (Os::Linux, Arch::X86) => &[],
            (Os::FreeBsd, Arch::X64) => &["freebsd-x86_64"],
            (Os::FreeBsd, Arch::Arm64) => &["freebsd-aarch64"],
            (Os::FreeBsd, _) => &[],
        }
    }

    /// Package formats the launcher installs on this OS, in order of preference.
    pub fn default_kinds(self) -> &'static [PackageKind] {
        match self.os {
            Os::Windows => &[PackageKind::MaxBundle],
            Os::MacOs => &[PackageKind::Dmg],
            Os::Linux => &[PackageKind::AppImage, PackageKind::TarGz],
            Os::FreeBsd => &[PackageKind::TarGz],
        }
    }

    /// Every format offered as a manual download on this OS (the web build's download menu).
    pub fn download_kinds(self) -> &'static [PackageKind] {
        match self.os {
            Os::Windows => &[PackageKind::MaxBundle],
            Os::MacOs => &[PackageKind::Dmg],
            Os::Linux => &[PackageKind::AppImage, PackageKind::Deb, PackageKind::Rpm, PackageKind::ArchPkg, PackageKind::Flatpak, PackageKind::TarGz],
            Os::FreeBsd => &[PackageKind::TarGz],
        }
    }
}

/// What an asset name says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetName {
    pub stem: String,
    pub version: String,
    pub token: String,
    pub kind: PackageKind,
}

impl AssetName {
    /// The architecture the asset is built for: the last part of its token (`windows-x86` → x86).
    /// `None` for a token that names none (`macos-universal`).
    pub fn arch(&self) -> Option<Arch> {
        match self.token.rsplit('-').next()? {
            "x64" | "x86_64" => Some(Arch::X64),
            "x86" => Some(Arch::X86),
            "arm64" | "aarch64" => Some(Arch::Arm64),
            "riscv64" => Some(Arch::Riscv64),
            _ => None,
        }
    }
}

/// Strips `<stem><sep>` (ASCII case-insensitively) from the start of `base`, returning the rest.
fn strip_stem_prefix<'a>(base: &'a str, stem: &str, sep: char) -> Option<&'a str> {
    if base.get(..stem.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(stem)) && base.as_bytes().get(stem.len()) == Some(&(sep as u8)) {
        Some(&base[stem.len() + 1..])
    } else {
        None
    }
}

/// Reads `<stem>-<version>-<os>-<arch>[-portable].<ext>` or Studio's `<Stem>_<version>_<arch>[_<locale>].<ext>`;
/// `None` for checksums, zsync files, CLI and web bundles, or anything else.
pub fn parse_asset_name(name: &str, stems: &[&str]) -> Option<AssetName> {
    const KINDS: [PackageKind; 10] = [
        PackageKind::MaxBundle,
        PackageKind::PortableZip,
        PackageKind::TarGz,
        PackageKind::Msi,
        PackageKind::Dmg,
        PackageKind::AppImage,
        PackageKind::Deb,
        PackageKind::Rpm,
        PackageKind::ArchPkg,
        PackageKind::Flatpak,
    ];
    let (kind, base) = KINDS.iter().find_map(|k| name.strip_suffix(k.suffix()).map(|b| (*k, b)))?;
    for stem in stems {
        if let Some(rest) = strip_stem_prefix(base, stem, '-') {
            // rest = <version>-<os>-<arch>; os and arch never contain '-', versions may.
            let mut parts = rest.rsplitn(3, '-');
            let (Some(arch), Some(os), Some(version)) = (parts.next(), parts.next(), parts.next()) else { continue };
            if crate::version::parse(version).is_none() || !version.starts_with(|c: char| c.is_ascii_digit()) {
                continue;
            }
            return Some(AssetName { stem: (*stem).to_string(), version: version.to_string(), token: format!("{os}-{arch}"), kind });
        }
        if let Some(rest) = strip_stem_prefix(base, stem, '_') {
            // Studio / Tauri format: <version>_<arch>[_<locale>] where <arch> may be `x86_64`.
            let Some((version, suffix)) = rest.split_once('_') else { continue };
            if crate::version::parse(version).is_none() || !version.starts_with(|c: char| c.is_ascii_digit()) {
                continue;
            }
            let arch_token = if suffix == "x86_64" || suffix.starts_with("x86_64_") { "x86_64" } else { suffix.split('_').next().unwrap_or("") };
            let token = match (kind, arch_token) {
                (PackageKind::Dmg, "universal") => Some("macos-universal".to_string()),
                (PackageKind::Dmg, "x86_64" | "x64") => Some("macos-x86_64".to_string()),
                (PackageKind::Dmg, "aarch64" | "arm64") => Some("macos-aarch64".to_string()),
                (PackageKind::Msi | PackageKind::PortableZip, "x86_64" | "x64") => Some("windows-x64".to_string()),
                (PackageKind::Msi | PackageKind::PortableZip, "x86") => Some("windows-x86".to_string()),
                (PackageKind::Msi | PackageKind::PortableZip, "arm64" | "aarch64") => Some("windows-arm64".to_string()),
                _ => None,
            };
            if let Some(token) = token {
                return Some(AssetName { stem: (*stem).to_string(), version: version.to_string(), token, kind });
            }
        }
    }
    None
}

/// The best asset of `release` for `platform`, trying `kinds` in order and, within a kind, the
/// platform's tokens in order (native before emulated).
pub fn pick_asset<'a>(release: &'a Release, stems: &[&str], platform: Platform, kinds: &[PackageKind]) -> Option<(&'a Asset, PackageKind)> {
    let parsed: Vec<(&Asset, AssetName)> = release.assets.iter().filter_map(|a| parse_asset_name(&a.name, stems).map(|n| (a, n))).collect();
    for kind in kinds {
        for token in platform.tokens() {
            if let Some((asset, _)) = parsed.iter().find(|(_, n)| n.kind == *kind && n.token == *token) {
                return Some((asset, *kind));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(names: &[&str]) -> Release {
        Release {
            assets: names.iter().map(|n| Asset { name: (*n).into(), size: 1, url: format!("https://x/{n}"), sha256: None }).collect(),
            ..Release::default()
        }
    }

    const PHOTO: &[&str] = &[
        "phototool-0.5.0-freebsd-x86_64.tar.gz",
        "phototool-0.5.0-linux-aarch64.AppImage",
        "phototool-0.5.0-linux-aarch64.AppImage.zsync",
        "phototool-0.5.0-linux-aarch64.deb",
        "phototool-0.5.0-linux-riscv64.tar.gz",
        "phototool-0.5.0-linux-x86_64.AppImage",
        "phototool-0.5.0-linux-x86_64.tar.gz",
        "phototool-0.5.0-macos-universal.dmg",
        "phototool-0.5.0-windows-x64-portable.zip",
        "phototool-0.5.0-windows-x64.msi",
        "phototool-0.5.0-windows-x86.msi",
        "phototool-cli-0.5.0-macos-universal.zip",
        "phototool-web-0.5.0.zip",
        "SHA256SUMS.txt",
    ];

    #[test]
    fn macos_prefers_the_published_universal_build() {
        // PhotoTool v0.9.0's GitHub metadata, trimmed to its desktop DMG.
        let mut r =
            crate::release::parse_github_release(include_str!("../tests/fixtures/phototool-release.json"), "carlesllavata/phototool").unwrap();
        // Split builds must not outrank the published universal DMG, even when listed first.
        r.assets.splice(0..0, release(&["phototool-0.9.0-macos-arm64.dmg", "phototool-0.9.0-macos-x86_64.dmg"]).assets);
        for arch in [Arch::X64, Arch::Arm64] {
            let (asset, kind) = pick_asset(&r, &["phototool"], Platform { os: Os::MacOs, arch }, &[PackageKind::Dmg]).unwrap();
            assert_eq!(asset.name, "phototool-0.9.0-macos-universal.dmg");
            assert_eq!(kind, PackageKind::Dmg);
        }
    }

    #[test]
    fn texturetool_selects_published_desktop_assets() {
        let release =
            crate::release::parse_github_release(include_str!("../tests/fixtures/texturetool-release.json"), "carlesllavata/texturetool").unwrap();
        for (os, kind, expected) in [
            (Os::Windows, PackageKind::Msi, "texturetool-0.1.0-windows-x64.msi"),
            (Os::MacOs, PackageKind::Dmg, "texturetool-0.1.0-macos-universal.dmg"),
            (Os::Linux, PackageKind::Flatpak, "texturetool-0.1.0-linux-x86_64.flatpak"),
        ] {
            let platform = Platform { os, arch: Arch::X64 };
            let (asset, selected) = pick_asset(&release, &["texturetool"], platform, &[kind]).unwrap();
            assert_eq!(asset.name, expected);
            assert_eq!(selected, kind);
        }
    }

    #[test]
    fn freebsd_picks_the_tarball_for_the_processor() {
        // SoundTool v0.10.0's GitHub metadata, trimmed to its FreeBSD tarballs.
        let r = crate::release::parse_github_release(include_str!("../tests/fixtures/soundtool-release.json"), "carlesllavata/soundtool").unwrap();
        let pick = |arch| {
            let platform = Platform { os: Os::FreeBsd, arch };
            pick_asset(&r, &["soundtool"], platform, platform.default_kinds()).map(|(a, k)| (a.name.as_str(), k))
        };
        assert_eq!(pick(Arch::X64), Some(("soundtool-0.10.0-freebsd-x86_64.tar.gz", PackageKind::TarGz)));
        assert_eq!(pick(Arch::Arm64), Some(("soundtool-0.10.0-freebsd-aarch64.tar.gz", PackageKind::TarGz)));
        for arch in [Arch::X86, Arch::Riscv64] {
            assert_eq!(pick(arch), None);
        }
        // An x86_64 machine never takes the ARM tarball, nor an ARM machine the x86_64 one.
        let only_arm = release(&["soundtool-0.10.0-freebsd-aarch64.tar.gz"]);
        assert_eq!(pick_asset(&only_arm, &["soundtool"], Platform { os: Os::FreeBsd, arch: Arch::X64 }, &[PackageKind::TarGz]), None);
        let only_x64 = release(&["soundtool-0.10.0-freebsd-x86_64.tar.gz"]);
        assert_eq!(pick_asset(&only_x64, &["soundtool"], Platform { os: Os::FreeBsd, arch: Arch::Arm64 }, &[PackageKind::TarGz]), None);
    }

    #[test]
    fn macos_split_builds_match_the_processor() {
        let r = release(&["phototool-0.5.0-macos-arm64.dmg", "phototool-0.5.0-macos-x86_64.dmg"]);
        let pick = |arch| pick_asset(&r, &["phototool"], Platform { os: Os::MacOs, arch }, &[PackageKind::Dmg]).map(|(a, _)| a.name.as_str());
        assert_eq!(pick(Arch::X64), Some("phototool-0.5.0-macos-x86_64.dmg"));
        assert_eq!(pick(Arch::Arm64), Some("phototool-0.5.0-macos-arm64.dmg"));
    }

    #[test]
    fn intel_macos_never_selects_an_arm_only_build() {
        for token in ["macos-aarch64", "macos-arm64"] {
            let name = format!("phototool-0.5.0-{token}.dmg");
            let r = release(&[&name]);
            assert_eq!(pick_asset(&r, &["phototool"], Platform { os: Os::MacOs, arch: Arch::X64 }, &[PackageKind::Dmg]), None);
            assert_eq!(
                pick_asset(&r, &["phototool"], Platform { os: Os::MacOs, arch: Arch::Arm64 }, &[PackageKind::Dmg]).map(|(a, _)| a.name.as_str()),
                Some(name.as_str())
            );
        }
    }

    #[test]
    fn apple_silicon_keeps_the_intel_fallback() {
        let r = release(&["phototool-0.5.0-macos-x86_64.dmg"]);
        assert_eq!(
            pick_asset(&r, &["phototool"], Platform { os: Os::MacOs, arch: Arch::Arm64 }, &[PackageKind::Dmg]).map(|(a, _)| a.name.as_str()),
            Some("phototool-0.5.0-macos-x86_64.dmg")
        );
        for arch in [Arch::X86, Arch::Riscv64] {
            assert_eq!(pick_asset(&r, &["phototool"], Platform { os: Os::MacOs, arch }, &[PackageKind::Dmg]), None);
        }
    }

    #[test]
    fn asset_names_carry_their_architecture() {
        let arch = |name: &str| parse_asset_name(name, &["phototool"]).unwrap().arch();
        assert_eq!(arch("phototool-0.5.0-windows-x64.msi"), Some(Arch::X64));
        assert_eq!(arch("phototool-0.5.0-windows-x86.msi"), Some(Arch::X86));
        assert_eq!(arch("phototool-0.5.0-linux-aarch64.tar.gz"), Some(Arch::Arm64));
        assert_eq!(arch("phototool-0.5.0-macos-universal.dmg"), None);
    }

    #[test]
    fn reads_asset_names() {
        let n = parse_asset_name("phototool-0.2.0-rc.1-windows-arm64-portable.zip", &["phototool"]).unwrap();
        assert_eq!((n.version.as_str(), n.token.as_str(), n.kind), ("0.2.0-rc.1", "windows-arm64", PackageKind::PortableZip));
        let n = parse_asset_name("printtool-0.2.1-linux-x86_64.tar.gz", &["pdftool", "printtool"]).unwrap();
        assert_eq!((n.stem.as_str(), n.kind), ("printtool", PackageKind::TarGz));
        for other in
            ["phototool-cli-0.5.0-macos-universal.zip", "phototool-web-0.5.0.zip", "SHA256SUMS.txt", "phototool-0.5.0-linux-x86_64.AppImage.zsync"]
        {
            assert_eq!(parse_asset_name(other, &["phototool"]), None, "{other}");
        }
        // FigTool also ships its collaboration server; that tarball is not the app.
        assert_eq!(parse_asset_name("figtool-server-0.5.1-linux-x86_64.tar.gz", &["figtool"]), None);
        assert!(parse_asset_name("figtool-0.5.1-linux-x86_64.tar.gz", &["figtool"]).is_some());
    }

    /// Studio's underscore-form MSI and DMG names parse cleanly, including `x86_64` and non-ASCII prefixes.
    #[test]
    fn reads_arttool_studio_asset_names() {
        let dmg = parse_asset_name("ArtTool_0.41.1_universal.dmg", &["arttool"]).unwrap();
        assert_eq!((dmg.version.as_str(), dmg.token.as_str(), dmg.kind), ("0.41.1", "macos-universal", PackageKind::Dmg));

        let x86_64_dmg = parse_asset_name("ArtTool_0.41.1_x86_64.dmg", &["arttool"]).unwrap();
        assert_eq!((x86_64_dmg.version.as_str(), x86_64_dmg.token.as_str(), x86_64_dmg.kind), ("0.41.1", "macos-x86_64", PackageKind::Dmg));

        let msi = parse_asset_name("ArtTool_0.41.1_x64_en-US.msi", &["arttool"]).unwrap();
        assert_eq!((msi.version.as_str(), msi.token.as_str(), msi.kind), ("0.41.1", "windows-x64", PackageKind::Msi));

        let x86_64_msi = parse_asset_name("ArtTool_0.41.1_x86_64.msi", &["arttool"]).unwrap();
        assert_eq!((x86_64_msi.version.as_str(), x86_64_msi.token.as_str(), x86_64_msi.kind), ("0.41.1", "windows-x64", PackageKind::Msi));

        assert_eq!(parse_asset_name("arttooé_0.41.1_x64.msi", &["arttool"]), None);
    }

    #[test]
    fn picks_the_native_build_first_and_falls_back_to_emulation() {
        let r = release(PHOTO);
        let pick = |os, arch, kinds: &[PackageKind]| pick_asset(&r, &["phototool"], Platform { os, arch }, kinds).map(|(a, _)| a.name.clone());
        assert_eq!(pick(Os::MacOs, Arch::Arm64, &[PackageKind::Dmg]).as_deref(), Some("phototool-0.5.0-macos-universal.dmg"));
        assert_eq!(pick(Os::Windows, Arch::X64, &[PackageKind::Msi]).as_deref(), Some("phototool-0.5.0-windows-x64.msi"));
        // No arm64 MSI in this release: Windows on ARM runs the x64 build.
        assert_eq!(pick(Os::Windows, Arch::Arm64, &[PackageKind::Msi]).as_deref(), Some("phototool-0.5.0-windows-x64.msi"));
        assert_eq!(pick(Os::Linux, Arch::Arm64, &[PackageKind::AppImage]).as_deref(), Some("phototool-0.5.0-linux-aarch64.AppImage"));
        assert_eq!(pick(Os::Linux, Arch::Arm64, &[PackageKind::TarGz]), None);
        assert_eq!(pick(Os::Linux, Arch::Riscv64, &[PackageKind::TarGz]).as_deref(), Some("phototool-0.5.0-linux-riscv64.tar.gz"));
        assert_eq!(pick(Os::FreeBsd, Arch::X64, &[PackageKind::TarGz]).as_deref(), Some("phototool-0.5.0-freebsd-x86_64.tar.gz"));
        // Preference order across kinds.
        assert_eq!(
            pick(Os::Windows, Arch::X64, &[PackageKind::PortableZip, PackageKind::Msi]).as_deref(),
            Some("phototool-0.5.0-windows-x64-portable.zip")
        );
        assert_eq!(pick(Os::Windows, Arch::X86, &[PackageKind::PortableZip, PackageKind::Msi]).as_deref(), Some("phototool-0.5.0-windows-x86.msi"));

        let p_riscv = Platform { os: Os::Linux, arch: Arch::Riscv64 };
        assert_eq!(
            pick_asset(&r, &["phototool"], p_riscv, p_riscv.default_kinds()).map(|(a, _)| a.name.clone()).as_deref(),
            Some("phototool-0.5.0-linux-riscv64.tar.gz")
        );
        assert_eq!(
            pick_asset(&r, &["phototool"], p_riscv, p_riscv.download_kinds()).map(|(a, _)| a.name.clone()).as_deref(),
            Some("phototool-0.5.0-linux-riscv64.tar.gz")
        );
        let r_riscv_deb =
            release(&["phototool-0.5.0-linux-riscv64.tar.gz", "phototool-0.5.0-linux-riscv64.deb", "phototool-0.5.0-linux-riscv64.rpm"]);
        assert_eq!(
            pick_asset(&r_riscv_deb, &["phototool"], p_riscv, p_riscv.download_kinds()).map(|(a, _)| a.name.clone()).as_deref(),
            Some("phototool-0.5.0-linux-riscv64.deb")
        );
    }

    #[test]
    fn strictly_matches_architecture_for_all_linux_package_formats() {
        let multi_arch_release = release(&[
            "phototool-0.5.0-linux-aarch64.AppImage",
            "phototool-0.5.0-linux-x86_64.AppImage",
            "phototool-0.5.0-linux-aarch64.deb",
            "phototool-0.5.0-linux-x86_64.deb",
            "phototool-0.5.0-linux-aarch64.rpm",
            "phototool-0.5.0-linux-x86_64.rpm",
            "phototool-0.5.0-linux-aarch64.pkg.tar.zst",
            "phototool-0.5.0-linux-x86_64.pkg.tar.zst",
            "phototool-0.5.0-linux-aarch64.flatpak",
            "phototool-0.5.0-linux-x86_64.flatpak",
            "phototool-0.5.0-linux-aarch64.tar.gz",
            "phototool-0.5.0-linux-x86_64.tar.gz",
        ]);

        let p_x64 = Platform { os: Os::Linux, arch: Arch::X64 };
        let p_arm64 = Platform { os: Os::Linux, arch: Arch::Arm64 };

        for kind in [PackageKind::AppImage, PackageKind::Deb, PackageKind::Rpm, PackageKind::ArchPkg, PackageKind::Flatpak, PackageKind::TarGz] {
            let pick_x64 = pick_asset(&multi_arch_release, &["phototool"], p_x64, &[kind]).map(|(a, _)| a.name.clone());
            assert!(pick_x64.as_deref().unwrap().contains("linux-x86_64"), "Expected x86_64 asset for {kind:?}, got {pick_x64:?}");

            let pick_arm64 = pick_asset(&multi_arch_release, &["phototool"], p_arm64, &[kind]).map(|(a, _)| a.name.clone());
            assert!(pick_arm64.as_deref().unwrap().contains("linux-aarch64"), "Expected aarch64 asset for {kind:?}, got {pick_arm64:?}");
        }

        // On x86_64 Linux, an aarch64-only asset must NEVER match or fall back
        let arm_only_release = release(&[
            "phototool-0.5.0-linux-aarch64.AppImage",
            "phototool-0.5.0-linux-aarch64.deb",
            "phototool-0.5.0-linux-aarch64.rpm",
            "phototool-0.5.0-linux-aarch64.pkg.tar.zst",
            "phototool-0.5.0-linux-aarch64.flatpak",
            "phototool-0.5.0-linux-aarch64.tar.gz",
        ]);
        for kind in [PackageKind::AppImage, PackageKind::Deb, PackageKind::Rpm, PackageKind::ArchPkg, PackageKind::Flatpak, PackageKind::TarGz] {
            assert_eq!(pick_asset(&arm_only_release, &["phototool"], p_x64, &[kind]), None, "x86_64 must never match an aarch64 asset for {kind:?}");
        }
    }
}
