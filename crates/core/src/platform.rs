// SPDX-License-Identifier: GPL-3.0-or-later
//! Platforms, package formats, and choosing the right release asset.
//!
//! Every Open3D app release names its files `<app>-<version>-<os>-<arch>[-portable|-3dsmax].<ext>`,
//! for example `opentools-0.5.1-windows-x64-3dsmax.zip` (a 3ds Max plug-in),
//! `phototool-0.1.0-windows-x64.zip` (a standalone app) or `open3d-launcher-0.2.0-windows-x64.msi`.
//!
//! Labels are English. The UI translates [`PackageKind::label`]; OS and architecture names are
//! shown as they are.

use serde::{Deserialize, Serialize};

use crate::release::{Asset, Release};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Os {
    Windows,
}

impl Os {
    pub fn label(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Arch {
    X64,
    X86,
    Arm64,
}

impl Arch {
    pub fn label(self) -> &'static str {
        match self {
            Self::X64 => "x64",
            Self::X86 => "x86",
            Self::Arm64 => "ARM64",
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
    /// Open3D: a 3ds Max plug-in bundle, a zip whose root holds `<App>.bundle`, unpacked into
    /// `ApplicationPlugins` (`<app>-<version>-windows-x64-3dsmax.zip`).
    MaxBundle,
    /// Open3D: a standalone Windows app, a zip whose root holds `<App>.exe`, unpacked into
    /// `Programs\Open3D\<App>` (`<app>-<version>-windows-x64.zip`).
    AppZip,
}

impl PackageKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Msi => "Installer (.msi)",
            Self::PortableZip => "Portable (.zip)",
            Self::MaxBundle => "3ds Max plug-in (.zip)",
            Self::AppZip => "Windows app (.zip)",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Msi => ".msi",
            Self::PortableZip => "-portable.zip",
            Self::MaxBundle => "-3dsmax.zip",
            Self::AppZip => ".zip",
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
        if !cfg!(target_os = "windows") {
            return None;
        }
        let os = Os::Windows;
        let arch = if cfg!(target_arch = "x86_64") {
            Arch::X64
        } else if cfg!(target_arch = "x86") {
            Arch::X86
        } else if cfg!(target_arch = "aarch64") {
            Arch::Arm64
        } else {
            return None;
        };
        Some(Self { os, arch })
    }

    pub fn label(self) -> String {
        format!("{} {}", self.os.label(), self.arch.label())
    }

    /// Asset platform tokens to try, best first. Windows on ARM and 64-bit Windows can fall back
    /// to emulated builds.
    fn tokens(self) -> &'static [&'static str] {
        match (self.os, self.arch) {
            (Os::Windows, Arch::X64) => &["windows-x64", "windows-x86"],
            (Os::Windows, Arch::X86) => &["windows-x86"],
            (Os::Windows, Arch::Arm64) => &["windows-arm64", "windows-x64", "windows-x86"],
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

/// Reads `<stem>-<version>-<os>-<arch>[-portable|-3dsmax].<ext>`;
/// `None` for checksums, zsync files, CLI and web bundles, or anything else.
pub fn parse_asset_name(name: &str, stems: &[&str]) -> Option<AssetName> {
    // The plain `.zip` of a standalone app comes after the zips with a longer suffix of their own.
    const KINDS: [PackageKind; 4] = [PackageKind::MaxBundle, PackageKind::PortableZip, PackageKind::AppZip, PackageKind::Msi];
    let (kind, base) = KINDS.iter().find_map(|k| name.strip_suffix(k.suffix()).map(|b| (*k, b)))?;
    for stem in stems {
        if let Some(rest) = strip_stem_prefix(base, stem, '-') {
            // rest = <version>-<os>-<arch>; os and arch never contain '-', versions may.
            let mut parts = rest.rsplitn(3, '-');
            let (Some(arch), Some(os), Some(version)) = (parts.next(), parts.next(), parts.next()) else { continue };
            if crate::version::parse(version).is_none() || !version.starts_with(|c: char| c.is_ascii_digit()) {
                continue;
            }
            // A plain `.zip` says nothing by its suffix, so it must end in a Windows platform:
            // `<app>-<version>-windows-x64-symbols.zip` is some other file, not version
            // `<version>-windows` built for `x64-symbols`.
            if kind == PackageKind::AppZip && os != "windows" {
                continue;
            }
            return Some(AssetName { stem: (*stem).to_string(), version: version.to_string(), token: format!("{os}-{arch}"), kind });
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

    #[test]
    fn asset_names_carry_their_architecture() {
        let arch = |name: &str| parse_asset_name(name, &["phototool"]).unwrap().arch();
        assert_eq!(arch("phototool-0.5.0-windows-x64.msi"), Some(Arch::X64));
        assert_eq!(arch("phototool-0.5.0-windows-x86.msi"), Some(Arch::X86));
        assert_eq!(arch("phototool-0.5.0-windows-arm64-portable.zip"), Some(Arch::Arm64));
        assert_eq!(arch("phototool-0.5.0-windows-x64-3dsmax.zip"), Some(Arch::X64));
    }

    #[test]
    fn a_plain_zip_is_a_standalone_app_and_never_swallows_the_other_zips() {
        let kind = |name: &str| parse_asset_name(name, &["phototool"]).map(|n| n.kind);
        assert_eq!(kind("phototool-0.5.0-windows-x64.zip"), Some(PackageKind::AppZip));
        assert_eq!(kind("phototool-0.5.0-beta.1-windows-x64.zip"), Some(PackageKind::AppZip));
        assert_eq!(kind("phototool-0.5.0-windows-x64-3dsmax.zip"), Some(PackageKind::MaxBundle));
        assert_eq!(kind("phototool-0.5.0-windows-x64-portable.zip"), Some(PackageKind::PortableZip));
        assert_eq!(kind("phototool-0.5.0-windows-x64.msi"), Some(PackageKind::Msi));
        // Source archives and zips with another suffix are not packages.
        assert_eq!(kind("phototool-0.5.0-windows-x64-symbols.zip"), None);
        assert_eq!(kind("phototool-0.5.0.zip"), None);
        assert_eq!(kind("phototool-src.zip"), None);
    }

    #[test]
    fn each_app_is_offered_only_its_own_kind_of_package() {
        let release = |names: &[&str]| Release {
            assets: names.iter().map(|n| Asset { name: (*n).into(), ..Asset::default() }).collect(),
            ..Release::default()
        };
        let platform = Platform { os: Os::Windows, arch: Arch::X64 };
        let both = release(&["phototool-0.5.0-windows-x64.zip", "phototool-0.5.0-windows-x64-3dsmax.zip"]);
        let picked = |kinds: &[PackageKind]| pick_asset(&both, &["phototool"], platform, kinds).map(|(a, k)| (a.name.clone(), k));
        assert_eq!(picked(&[PackageKind::AppZip]), Some(("phototool-0.5.0-windows-x64.zip".into(), PackageKind::AppZip)));
        assert_eq!(picked(&[PackageKind::MaxBundle]), Some(("phototool-0.5.0-windows-x64-3dsmax.zip".into(), PackageKind::MaxBundle)));
        let plugin_only = release(&["phototool-0.5.0-windows-x64-3dsmax.zip"]);
        assert!(pick_asset(&plugin_only, &["phototool"], platform, &[PackageKind::AppZip]).is_none());
    }
}
