// SPDX-License-Identifier: GPL-3.0-or-later
//! The Open3D suite: bundled data with a signed hosted startup snapshot.
//!
//! The list is data so a later version can fetch it from an API; [`SUITE`] is the built-in copy.
//!
//! Text here is English, the source the UI translates from: each `tagline` has a catalog row
//! keyed by the app id (`tr_ctx(app.id, app.tagline)` in `ui-egui`).

/// Who makes an app. Official apps live under [`GITHUB_OWNER`]; a community app is maintained
/// outside Open3D and is labelled as such wherever it is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppKind {
    Official,
    Community,
}

/// What an app is on disk, which decides its package, where it is installed and how it is opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppPackage {
    /// A 3ds Max plug-in bundle in `ApplicationPlugins`; opening it starts 3ds Max.
    MaxBundle,
    /// A program of its own (`<App>.exe`) in its folder under `Programs\Open3D`.
    Standalone,
}

impl AppPackage {
    /// The catalog's word for it (`package = "standalone"`); a missing field means a plug-in.
    pub fn from_catalog(word: Option<&str>) -> Option<Self> {
        match word {
            None | Some("3dsmax") => Some(Self::MaxBundle),
            Some("standalone") => Some(Self::Standalone),
            Some(_) => None,
        }
    }

    /// The package formats an app of this kind is installed from, best first.
    pub fn kinds(self) -> &'static [crate::PackageKind] {
        match self {
            Self::MaxBundle => &[crate::PackageKind::MaxBundle],
            Self::Standalone => &[crate::PackageKind::AppZip],
        }
    }
}

/// One app in the suite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuiteApp {
    /// Machine name: repo, binary, package and asset stem (`phototool`).
    pub id: &'static str,
    /// Display name (`PhotoTool`).
    pub name: &'static str,
    /// What it's for, in a few words (English; the UI translates it by app id).
    pub tagline: &'static str,
    /// Brand colour (sRGB), for surfaces: github.com/carlesllavata/Open3D's `--app`.
    pub color: [u8; 3],
    /// The brand colour for text on a light background (the site's light `--app-ink`).
    pub ink_light: [u8; 3],
    /// The brand colour for text on a dark background (the site's dark `--app-ink`).
    pub ink_dark: [u8; 3],
    /// GitHub `owner/repo`.
    pub repo: &'static str,
    /// Older names its release assets may carry (PdfTool shipped as PrintTool until 0.2.1).
    pub former_ids: &'static [&'static str],
    /// Older display names its installers registered (PdfTool's MSI said PrintTool until 0.2.1).
    pub former_names: &'static [&'static str],
    /// The `.app` bundle stem on macOS when it differs from `name` (`Open3D` for `Open3D Studio`).
    pub mac_bundle_name: Option<&'static str>,
    pub kind: AppKind,
    /// Who publishes it, as the card's byline shows it (`Open3D`, `Tangency Labs`).
    pub publisher: &'static str,
    /// The publisher its Windows installer names, when it isn't Open3D's (`Open3D`).
    pub msi_publisher: Option<&'static str>,
    /// The bundle id when it isn't `io.github.carlesllavata.<id>` (a community app brings its own).
    pub bundle: Option<&'static str>,
    /// Older bundle ids its Flatpak shipped under (SolidTool was `io.github.carlesllavata.solidtool`).
    pub former_bundles: &'static [&'static str],
    /// Its website, when it isn't a page on github.com/carlesllavata/Open3D (a community app has its own).
    pub site: Option<&'static str>,
    /// The Apple Developer team whose Developer ID signature its macOS build must carry. `None`
    /// while those builds carry no signature the launcher can check: it then opens a copy that's
    /// there and links to the releases, but installs nothing.
    pub mac_team: Option<&'static str>,
    /// A 3ds Max plug-in or a program of its own.
    pub package: AppPackage,
}

impl SuiteApp {
    /// The package formats this app is installed from, best first.
    pub fn kinds(&self) -> &'static [crate::PackageKind] {
        self.package.kinds()
    }

    /// Whether the app is a program of its own rather than a 3ds Max plug-in.
    pub fn is_standalone(&self) -> bool {
        self.package == AppPackage::Standalone
    }

    /// `io.github.carlesllavata.<id>`: macOS bundle id, Flatpak id and Linux desktop-file id.
    pub fn bundle_id(&self) -> String {
        self.bundle.map_or_else(|| format!("io.github.carlesllavata.{}", self.id), str::to_string)
    }

    pub fn releases_url(&self) -> String {
        format!("https://github.com/{}/releases", self.repo)
    }

    pub fn repo_url(&self) -> String {
        format!("https://github.com/{}", self.repo)
    }

    /// The app's website: its page on github.com/carlesllavata/Open3D, or a community app's own site.
    pub fn website(&self) -> String {
        self.site.map_or_else(|| format!("https://github.com/carlesllavata/{}", self.id), str::to_string)
    }

    /// Asset stems this app's releases may use, current first.
    pub fn stems(&self) -> impl Iterator<Item = &'static str> + '_ {
        std::iter::once(self.id).chain(self.former_ids.iter().copied())
    }

    /// Display names this app's installers may have registered, current first.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        std::iter::once(self.name).chain(self.former_names.iter().copied())
    }

    /// The primary `.app` bundle stem on macOS (`Open3D` or `PhotoTool`).
    pub fn mac_bundle_name(&self) -> &'static str {
        self.mac_bundle_name.unwrap_or(self.name)
    }

    pub fn by_id(id: &str) -> Option<&'static SuiteApp> {
        crate::hosted_catalog::apps().iter().find(|a| a.id == id)
    }
}

const fn app(
    id: &'static str,
    name: &'static str,
    tagline: &'static str,
    [color, ink_light, ink_dark]: [[u8; 3]; 3],
    repo: &'static str,
) -> SuiteApp {
    SuiteApp {
        id,
        name,
        tagline,
        color,
        ink_light,
        ink_dark,
        repo,
        former_ids: &[],
        former_names: &[],
        mac_bundle_name: None,
        kind: AppKind::Official,
        publisher: "Open3D",
        msi_publisher: None,
        bundle: None,
        former_bundles: &[],
        site: None,
        mac_team: None,
        package: AppPackage::MaxBundle,
    }
}

// The bundled suite, in website display order, then the approved community apps.
include!(concat!(env!("OUT_DIR"), "/catalog.rs"));

/// Open3D's home page: the launcher's public repository.
pub const WEBSITE_URL: &str = "https://github.com/carlesllavata/Open3D";
/// The GitHub account every official Open3D app's repository lives under. The release feed can
/// only move an app within it.
pub const GITHUB_OWNER: &str = "carlesllavata";
/// Open3D on GitHub: every Open3D app's source.
pub const GITHUB_URL: &str = "https://github.com/carlesllavata";
/// Social accounts (service, URL), shown as small icons. Open3D has none.
pub const SOCIAL: [(&str, &str); 0] = [];

/// Where to ask for help and report problems: the
/// launcher repository's issues.
pub const COMMUNITY_URL: &str = "https://github.com/carlesllavata/Open3D/issues";

/// Open3D Launcher itself, described like an Open3D app so it can update itself with the same
/// machinery (asset names `open3d-launcher-<ver>-…`). Not part of [`SUITE`].
pub const LAUNCHER: SuiteApp = app(
    "open3d-launcher",
    "Open3D Launcher",
    "Installs and updates the Open3D apps for 3ds Max",
    [[0x24, 0x26, 0x2b], [0x24, 0x26, 0x2b], [0xff, 0x8c, 0x1a]],
    "carlesllavata/Open3D",
);

/// The launcher's own asset stem.
pub const LAUNCHER_ID: &str = "open3d-launcher";

/// An Open3D app's 128 px icon (PNG), from `assets/app-icons`: the UI draws it.
pub fn app_icon_png(id: &str) -> Option<&'static [u8]> {
    if let Some(icon) = crate::hosted_catalog::icon(id) {
        return Some(icon);
    }
    Some(match id {
        "opentools" => include_bytes!("../../../assets/app-icons/opentools.png"),
        "openmanager" => include_bytes!("../../../assets/app-icons/openmanager.png"),
        "openview" => include_bytes!("../../../assets/app-icons/openview.png"),
        "openelements" => include_bytes!("../../../assets/app-icons/openelements.png"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_app_has_its_icon() {
        for app in &SUITE {
            let png = app_icon_png(app.id).unwrap_or_else(|| panic!("no icon for {}", app.id));
            assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "{} icon isn't a PNG", app.id);
        }
        assert!(app_icon_png("notatool").is_none());
    }

    #[test]
    fn the_catalog_names_two_kinds_of_app() {
        assert_eq!(AppPackage::from_catalog(None), Some(AppPackage::MaxBundle));
        assert_eq!(AppPackage::from_catalog(Some("3dsmax")), Some(AppPackage::MaxBundle));
        assert_eq!(AppPackage::from_catalog(Some("standalone")), Some(AppPackage::Standalone));
        assert_eq!(AppPackage::from_catalog(Some("installer")), None);
        assert_eq!(AppPackage::Standalone.kinds(), [crate::PackageKind::AppZip]);
        assert_eq!(AppPackage::MaxBundle.kinds(), [crate::PackageKind::MaxBundle]);
    }

    #[test]
    fn openview_is_the_only_standalone_app_in_the_catalog() {
        let standalone: Vec<&str> = SUITE.iter().filter(|a| a.is_standalone()).map(|a| a.id).collect();
        assert_eq!(standalone, ["openview"]);
        let openview = SUITE.iter().find(|a| a.id == "openview").unwrap();
        assert_eq!((openview.name, openview.repo), ("OpenView", "carlesllavata/OpenView"));
        assert_eq!(openview.kinds(), [crate::PackageKind::AppZip]);
    }
}
