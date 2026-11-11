// SPDX-License-Identifier: GPL-3.0-or-later
//! The user's choices, saved between runs by the shell.

use std::collections::BTreeMap;

use open3d_launcher_core::PackageKind;
use serde::{Deserialize, Serialize};

use crate::i18n::t;
use crate::theme::ThemeChoice;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewMode {
    #[default]
    Grid,
    Table,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Filter {
    #[default]
    All,
    // These three no longer appear in the toolbar but must remain deserializable from old settings.
    Installed,
    Updates,
    NotInstalled,
    /// Apps the user chose to hide from the other lists.
    Hidden,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortKey {
    /// Suite order: classic apps, then new. Settings saved with the removed "Inspired by" sort load as this.
    #[default]
    #[serde(alias = "InspiredBy")]
    Suite,
    Name,
    Status,
    /// Installed version.
    Installed,
    /// Latest released version.
    Latest,
    /// Release date.
    Released,
    /// How often the app has been opened from here, most first.
    Frequent,
}

impl SortKey {
    /// The direction a first click sorts in: newest and highest first for dates and versions,
    /// A to Z for text.
    pub fn starts_descending(self) -> bool {
        matches!(self, Self::Installed | Self::Latest | Self::Released | Self::Frequent)
    }
}

/// How often to look for updates in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckEvery {
    Hour,
    FourHours,
    Day,
    Never,
}

impl CheckEvery {
    /// The choices offered. `FourHours` and `Day` are kept so saved settings still load; they now
    /// mean "automatically" too.
    pub const ALL: [Self; 2] = [Self::Hour, Self::Never];

    pub fn label(self) -> String {
        match self {
            Self::Hour | Self::FourHours | Self::Day => t("Automatically").to_string(),
            Self::Never => t("Only when I ask").to_string(),
        }
    }

    /// How often to look. Checks ask Open3D's release feed (one cheap request; GitHub only as a
    /// fallback), so automatic checks run every twelve minutes to reduce GitHub API traffic.
    pub fn seconds(self) -> Option<i64> {
        match self {
            Self::Hour | Self::FourHours | Self::Day => Some(12 * 60),
            Self::Never => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub install_locations: open3d_launcher_core::protocol::InstallLocations,
    pub view: ViewMode,
    pub filter: Filter,
    pub sort: SortKey,
    pub sort_descending: bool,
    pub theme: ThemeChoice,
    /// Interface language: `auto` (follow the system) or a language code from the registry
    /// (`en`, `ru`, `zh-CN`, `de`, `fr`, `pt-BR`, `pl`).
    pub language: String,
    /// Closing the window keeps the launcher in the tray (menu bar) instead of quitting.
    pub close_to_tray: bool,
    /// macOS: showing the window brings it to the current desktop (Space) instead of switching to
    /// the desktop it was on.
    pub show_on_current_desktop: bool,
    pub check_every: CheckEvery,
    /// Automatically download and install updates when found.
    #[serde(default)]
    pub auto_update: bool,
    /// Update all apps together behind one permission prompt where the platform asks for one
    /// (Linux and Windows); off, each app's update asks on its own.
    #[serde(default)]
    pub batch_updates: bool,
    /// Show a system notification when an installed app has an update.
    pub notify: bool,
    pub prereleases: bool,
    /// Version track per app. Missing entries follow stable releases.
    pub versions: BTreeMap<String, open3d_launcher_core::protocol::VersionTrack>,
    pub launcher_version: open3d_launcher_core::protocol::VersionTrack,
    /// Linux: the package format to prefer for new installs (empty: the system's own).
    pub preferred_kinds: Vec<PackageKind>,
    /// Announcements and alerts from the release feed the user dismissed (by notice id).
    pub dismissed_notices: std::collections::BTreeSet<String>,
    /// Updates already announced, so each version is announced once: app id → version.
    pub notified: BTreeMap<String, String>,
    /// Unix seconds of the last finished check.
    pub last_check: Option<i64>,
    /// Hide community apps from the catalog without changing their installation or update settings.
    pub hide_community_apps: bool,
    /// Catalog ids of apps hidden from the main lists (`phototool`, …).
    pub hidden: std::collections::BTreeSet<String>,
    /// The one-time notice that closing the window keeps the launcher in the tray has been shown.
    pub tray_hint_shown: bool,
    /// How many times each app was opened from the launcher (catalog id → count), for "Frequently used".
    pub launches: BTreeMap<String, u32>,
    /// Catalog ids pinned to the top of the grid/table, in the order the user chose.
    pub pinned: Vec<String>,
    /// Extra command-line flags each app is opened with (catalog id → the text as typed, split
    /// by [`args::split`](open3d_launcher_core::args::split) at launch).
    pub launch_flags: BTreeMap<String, String>,
}

impl Settings {
    /// Machine-readable code for "follow the system language".
    pub const LANGUAGE_AUTO: &str = "auto";

    /// Clear saved catalog filters whose controls were removed from the toolbar.
    pub fn migrate(&mut self) {
        if !matches!(self.filter, Filter::All | Filter::Hidden) {
            self.filter = Filter::All;
        }
    }

    /// Whether `id` is in the pinned list.
    pub fn is_pinned(&self, id: &str) -> bool {
        self.pinned.iter().any(|p| p == id)
    }

    /// Pin `id` at the end of the pinned list (no-op if already pinned).
    pub fn pin_app(&mut self, id: &str) {
        if id.is_empty() || self.is_pinned(id) {
            return;
        }
        self.pinned.push(id.to_string());
    }

    /// Remove `id` from the pinned list.
    pub fn unpin_app(&mut self, id: &str) {
        self.pinned.retain(|p| p != id);
    }

    /// The index of the next pinned id above (`delta = -1`) or below (`delta = 1`) `id` that passes
    /// `available`. Skips stale catalog ids left in `pinned` so Move up/down still change the
    /// visible order.
    pub fn pinned_neighbor(&self, id: &str, delta: isize, available: impl Fn(&str) -> bool) -> Option<usize> {
        let mut j = self.pinned.iter().position(|p| p == id)?;
        loop {
            j = j.checked_add_signed(delta)?;
            if available(self.pinned.get(j)?) {
                return Some(j);
            }
        }
    }

    /// Move `from_id` to insertion slot `to_index` (`0..=len`, before remove). Used by drag-and-drop.
    pub fn reorder_pinned_to(&mut self, from_id: &str, mut to_index: usize) {
        let Some(from) = self.pinned.iter().position(|p| p == from_id) else {
            return;
        };
        to_index = to_index.min(self.pinned.len());
        // Already in this slot (including the no-op "insert immediately after self").
        if to_index == from || to_index == from + 1 {
            return;
        }
        let item = self.pinned.remove(from);
        if from < to_index {
            to_index -= 1;
        }
        self.pinned.insert(to_index, item);
    }

    /// Display name for a language `code`: a language's name in itself, from the registry
    /// ([`LANGUAGES`](crate::i18n::LANGUAGES)). A language needing a CJK font is named in English,
    /// with its own name added only while `installed` — the CJK face in the current font stack — is
    /// that language (no other face draws it); "Auto" for `auto` and unknown codes, like
    /// [`Lang::from_pref`](crate::i18n::Lang::from_pref).
    pub fn language_label(code: &str, installed: Option<crate::i18n::Lang>) -> String {
        match crate::i18n::Lang::from_code(code) {
            Some(lang) if lang.needs_cjk_font() && installed == Some(lang) => format!("{} ({})", lang.english_name(), lang.name()),
            Some(lang) if lang.needs_cjk_font() => lang.english_name().to_string(),
            Some(lang) => lang.name().to_string(),
            None => t("Auto").to_string(),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            install_locations: Default::default(),
            view: ViewMode::Grid,
            filter: Filter::All,
            sort: SortKey::Suite,
            sort_descending: false,
            theme: ThemeChoice::System,
            language: "auto".to_string(),
            close_to_tray: true,
            show_on_current_desktop: true,
            check_every: CheckEvery::Hour,
            auto_update: false,
            batch_updates: false,
            notify: true,
            prereleases: false,
            versions: BTreeMap::new(),
            launcher_version: Default::default(),
            preferred_kinds: Vec::new(),
            dismissed_notices: std::collections::BTreeSet::new(),
            notified: BTreeMap::new(),
            last_check: None,
            hide_community_apps: false,
            hidden: std::collections::BTreeSet::new(),
            tray_hint_shown: false,
            launches: BTreeMap::new(),
            pinned: Vec::new(),
            launch_flags: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_appends_once_and_unpin_removes() {
        let mut s = Settings::default();
        s.pin_app("phototool");
        s.pin_app("phototool");
        s.pin_app("filmtool");
        assert_eq!(s.pinned, vec!["phototool", "filmtool"]);
        s.unpin_app("phototool");
        assert_eq!(s.pinned, vec!["filmtool"]);
        s.unpin_app("missing");
        assert_eq!(s.pinned, vec!["filmtool"]);
    }

    #[test]
    fn reorder_pinned_to_uses_insertion_slot() {
        let mut s = Settings { pinned: vec!["a".into(), "b".into(), "c".into()], ..Settings::default() };
        // Insert c before a (slot 0).
        s.reorder_pinned_to("c", 0);
        assert_eq!(s.pinned, vec!["c", "a", "b"]);
        // Insert c after b (slot 3).
        s.reorder_pinned_to("c", 3);
        assert_eq!(s.pinned, vec!["a", "b", "c"]);
        // Insert a before c (slot 2) → [b, a, c].
        s.reorder_pinned_to("a", 2);
        assert_eq!(s.pinned, vec!["b", "a", "c"]);
        // Insert a after itself (slot 2) is a no-op, and so is an unknown id; a slot past the end
        // means the end.
        s.reorder_pinned_to("a", 2);
        s.reorder_pinned_to("missing", 0);
        assert_eq!(s.pinned, vec!["b", "a", "c"]);
        s.reorder_pinned_to("b", 99);
        assert_eq!(s.pinned, vec!["a", "c", "b"]);
    }

    #[test]
    fn old_settings_json_loads_with_empty_pinned() {
        let s: Settings = serde_json::from_str(r#"{"view":"Grid"}"#).expect("legacy settings");
        assert!(s.pinned.is_empty());
    }

    #[test]
    fn pinned_neighbor_steps_within_bounds() {
        let s = Settings { pinned: vec!["a".into(), "b".into(), "c".into()], ..Settings::default() };
        let any = |_: &str| true;
        assert_eq!(s.pinned_neighbor("b", -1, any), Some(0));
        assert_eq!(s.pinned_neighbor("b", 1, any), Some(2));
        assert_eq!(s.pinned_neighbor("a", -1, any), None);
        assert_eq!(s.pinned_neighbor("c", 1, any), None);
        assert_eq!(s.pinned_neighbor("missing", 1, any), None);
    }

    #[test]
    fn pinned_neighbor_skips_unavailable_ids() {
        let s = Settings { pinned: vec!["a".into(), "gone".into(), "b".into()], ..Settings::default() };
        let live = |id: &str| id != "gone";
        assert_eq!(s.pinned_neighbor("a", 1, live), Some(2));
        assert_eq!(s.pinned_neighbor("b", -1, live), Some(0));
    }
}
