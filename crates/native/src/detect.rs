// SPDX-License-Identifier: GPL-3.0-or-later
//! Finding Open3D apps that are already installed: the launcher's own record of what it installed,
//! else a 3ds Max bundle in `ApplicationPlugins`, whoever put it there.

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
