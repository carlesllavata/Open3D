// SPDX-License-Identifier: GPL-3.0-or-later
//! The version each app should follow, shared by the launcher and the apps.
//!
//! The launcher writes one small file when someone chooses a version in "Choose version"; an app's
//! own update check reads it, so an app kept on an older release, or on nightlies, is not offered
//! the latest stable release every time it starts. Only the launcher writes the file; apps read.
//!
//! The file sits in a folder of its own, `Open3D\versions` under `%LOCALAPPDATA%`, never in the
//! launcher's own folder.
//!
//! Invariants:
//! - A missing, unreadable or damaged file means every app follows stable releases, as before.
//! - An entry this version cannot read (written by a newer launcher) counts as managed by the
//!   launcher, never as stable, and [`Pins::save`] writes it back unchanged.
//! - Choosing stable removes the app's entry, so the file only lists apps kept on something else.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Names a file to use in place of the default (absolute paths only). The launcher sets it for an
/// app whose home it redirects (tarball installs on Linux), so the app still finds the real file.
pub const PATH_ENV: &str = "CRAFT_VERSIONS_FILE";

/// Which published build an app follows. A pinned version never changes on its own.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VersionTrack {
    #[default]
    Stable,
    Nightly,
    Pinned(String),
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    /// App id to its track, kept as raw values so entries from a newer launcher survive a save.
    #[serde(default)]
    apps: BTreeMap<String, serde_json::Value>,
}

/// The saved version choices.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pins {
    apps: BTreeMap<String, serde_json::Value>,
}

/// What an app's own update check should do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Follow stable releases: check and offer as usual.
    Stable,
    /// The launcher keeps this app on another track; the app offers nothing itself.
    Managed(VersionTrack),
    /// An entry this version cannot read; also left to the launcher.
    Unknown,
}

impl Pins {
    /// The shared file (see the module notes), or the one [`PATH_ENV`] names.
    pub fn default_path() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os(PATH_ENV).map(PathBuf::from).filter(|p| p.is_absolute()) {
            return Some(path);
        }
        crate::folders::host_data_local_dir().map(|d| d.join("Open3D").join("versions").join("versions.json"))
    }

    /// Reads `path`. A missing or damaged file reads as empty: every app follows stable releases.
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                log::warn!("couldn't read {} ({e}); every app follows stable releases", path.display());
                return Self::default();
            }
        };
        match serde_json::from_str::<File>(&text) {
            Ok(file) => Self { apps: file.apps },
            Err(e) => {
                log::warn!("{} is damaged ({e}); every app follows stable releases", path.display());
                Self::default()
            }
        }
    }

    /// Writes `path` in one step, so a reader never sees half a file.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let file = File { apps: self.apps.clone() };
        let text = serde_json::to_string_pretty(&file).map_err(|e| format!("couldn't encode the version choices ({e})"))?;
        crate::paths::write_atomic(path, text.as_bytes())
    }

    /// What `app`'s own update check should do.
    pub fn choice(&self, app: &str) -> Choice {
        match self.apps.get(app) {
            None => Choice::Stable,
            Some(raw) => match serde_json::from_value::<VersionTrack>(raw.clone()) {
                Ok(VersionTrack::Stable) => Choice::Stable,
                Ok(track) => Choice::Managed(track),
                Err(_) => Choice::Unknown,
            },
        }
    }

    /// The track `app` follows, when this version can read it.
    pub fn track(&self, app: &str) -> Option<VersionTrack> {
        match self.choice(app) {
            Choice::Stable => Some(VersionTrack::Stable),
            Choice::Managed(track) => Some(track),
            Choice::Unknown => None,
        }
    }

    /// Sets `app`'s track. Stable removes the entry.
    pub fn set(&mut self, app: &str, track: &VersionTrack) {
        match track {
            VersionTrack::Stable => {
                self.apps.remove(app);
            }
            other => {
                if let Ok(raw) = serde_json::to_value(other) {
                    self.apps.insert(app.to_string(), raw);
                }
            }
        }
    }

    /// Every app with a track this version can read, stable ones excluded.
    pub fn tracks(&self) -> impl Iterator<Item = (&str, VersionTrack)> {
        self.apps
            .iter()
            .filter_map(|(id, raw)| serde_json::from_value(raw.clone()).ok().map(|t| (id.as_str(), t)))
            .filter(|(_, track)| *track != VersionTrack::Stable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pinned(tag: &str) -> VersionTrack {
        VersionTrack::Pinned(tag.into())
    }

    #[test]
    fn a_missing_or_damaged_file_means_every_app_follows_stable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("versions.json");
        assert_eq!(Pins::load(&path).choice("pdftool"), Choice::Stable);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Pins::load(&path).choice("pdftool"), Choice::Stable);
        std::fs::write(&path, r#"{"apps":"wrong shape"}"#).unwrap();
        assert_eq!(Pins::load(&path).choice("pdftool"), Choice::Stable);
    }

    #[test]
    fn choices_survive_a_save_and_only_the_named_app_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("versions.json");
        let mut pins = Pins::default();
        pins.set("pdftool", &pinned("v0.13.0"));
        pins.set("phototool", &VersionTrack::Nightly);
        pins.save(&path).unwrap();
        let read = Pins::load(&path);
        assert_eq!(read, pins);
        assert_eq!(read.choice("pdftool"), Choice::Managed(pinned("v0.13.0")));
        assert_eq!(read.choice("phototool"), Choice::Managed(VersionTrack::Nightly));
        assert_eq!(read.choice("wordtool"), Choice::Stable);
        assert_eq!(read.tracks().count(), 2);
    }

    #[test]
    fn choosing_stable_removes_the_entry() {
        let mut pins = Pins::default();
        pins.set("pdftool", &pinned("v0.13.0"));
        pins.set("pdftool", &VersionTrack::Stable);
        assert_eq!(pins, Pins::default());
        assert_eq!(pins.track("pdftool"), Some(VersionTrack::Stable));
    }

    #[test]
    fn the_file_uses_the_launchers_own_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("versions.json");
        std::fs::write(&path, r#"{"apps":{"pdftool":{"Pinned":"v0.13.0"},"filmtool":"Nightly","wordtool":"Stable"}}"#).unwrap();
        let pins = Pins::load(&path);
        assert_eq!(pins.track("pdftool"), Some(pinned("v0.13.0")));
        assert_eq!(pins.track("filmtool"), Some(VersionTrack::Nightly));
        assert_eq!(pins.choice("wordtool"), Choice::Stable);
        let listed: Vec<&str> = pins.tracks().map(|(id, _)| id).collect();
        assert_eq!(listed, ["filmtool", "pdftool"], "an explicit Stable entry is not listed");
    }

    #[test]
    fn an_entry_from_a_newer_launcher_is_left_to_the_launcher_and_kept_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("versions.json");
        std::fs::write(&path, r#"{"apps":{"pdftool":{"Beta":{"since":"v0.20.0"}}}}"#).unwrap();
        let mut pins = Pins::load(&path);
        assert_eq!(pins.choice("pdftool"), Choice::Unknown);
        assert_eq!(pins.track("pdftool"), None);
        pins.set("wordtool", &pinned("v1.0.0"));
        pins.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Beta") && text.contains("v0.20.0"), "kept: {text}");
        assert_eq!(Pins::load(&path).choice("pdftool"), Choice::Unknown);
    }
}
