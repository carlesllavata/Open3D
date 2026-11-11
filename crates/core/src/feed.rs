// SPDX-License-Identifier: GPL-3.0-or-later
//! An optional release feed: one JSON document listing every app's latest release, served by a
//! service that polls GitHub on a schedule so the launcher doesn't have to (GitHub allows 60
//! unauthenticated requests an hour). Open3D runs no such service ([`FEED_URL`] is empty), so
//! GitHub answers for every app; the feed is still read from a developer's local mirror. GitHub
//! stays the fallback: for apps the feed doesn't list, apps it hasn't fetched within [`MAX_AGE`],
//! or when the feed doesn't answer.
//!
//! The feed is input like any other: every field is optional, unknown fields are ignored, a field
//! of the wrong type reads as missing, one bad entry never spoils the others, and download links
//! must still point at the app's own GitHub releases.
//!
//! A developer can serve the feed and the builds from their own machine instead (see
//! [`Feed::with_mirror`]): then links under the mirror are accepted too, and entries never go stale.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer};

use crate::catalog::{GITHUB_OWNER, SuiteApp};
use crate::release::{self, Asset, Release, truncate};
use crate::time::parse_rfc3339;
use crate::version;

/// Where the feed lives.
/// Open3D runs no release-feed service: empty means every app's releases come from GitHub. A
/// developer can still point `OPEN3D_LAUNCHER_FEED_URL` at a local mirror.
pub const FEED_URL: &str = "";

/// An app's entry is trusted for this long after the feed fetched it.
pub const MAX_AGE: i64 = 24 * 3600;

/// The feed, read leniently.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Feed {
    /// By lowercase app name.
    pub apps: BTreeMap<String, FeedApp>,
    /// When the least recently fetched app was fetched (Unix seconds).
    pub last_fetched: Option<i64>,
    pub app_renames: Vec<AppRename>,
    /// Repositories that moved, both under [`GITHUB_OWNER`]: a rename anywhere else is dropped.
    pub repo_renames: Vec<RepoRename>,
    pub announcements: Vec<Notice>,
    pub alerts: Vec<Notice>,
    /// A local mirror's address (ending in `/`): downloads under it are accepted alongside GitHub's.
    /// Never read from the feed itself; the caller sets it with [`Feed::with_mirror`].
    pub mirror: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedApp {
    pub repo: Option<String>,
    /// Present when the repo has a published release. Meaningless unless `fetched_at` is set.
    pub release: Option<FeedRelease>,
    /// When the feed last fetched this app (Unix seconds).
    pub fetched_at: Option<i64>,
}

/// A release as the feed describes it, before it's checked against the app's repo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedRelease {
    pub tag: String,
    pub name: String,
    pub published_at: Option<i64>,
    pub html_url: String,
    pub prerelease: bool,
    pub notes: String,
    pub assets: Vec<(String, u64, String, Option<String>)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppRename {
    pub old_name: String,
    pub old_name_lower: String,
    pub new_name: String,
    pub new_name_lower: String,
    pub description: String,
    pub renamed_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoRename {
    pub old_repo: String,
    pub new_repo: String,
    pub renamed_at: Option<i64>,
}

/// An announcement (shown inline, dismissible) or an alert (a popup the user acknowledges).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notice {
    /// Stable, so a dismissal sticks across launches and refreshes.
    pub id: String,
    pub title: String,
    pub body: String,
    /// Only `https://` links survive parsing.
    pub link: Option<String>,
    pub created_at: Option<i64>,
}

/// What the feed says about one app, once checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedAnswer {
    /// Fresh: the app's latest release, or `None` when it has none.
    Fresh(Option<Release>),
    /// Not listed, never fetched, or older than [`MAX_AGE`]: ask GitHub.
    AskGitHub,
}

const MAX_NOTICES: usize = 20;
const MAX_TEXT: usize = 4000;

impl Feed {
    /// Reads the feed. `None` only when the answer isn't JSON (or isn't an object) at all.
    pub fn parse(json: &str) -> Option<Self> {
        let raw: RawResponse = serde_json::from_str(json).ok()?;
        let mut feed = Feed {
            apps: raw.release_info.apps.into_iter().map(|(k, v)| (k.to_lowercase(), v.into_app())).collect(),
            last_fetched: raw.release_info.last_fetched.as_deref().and_then(parse_rfc3339),
            app_renames: raw.app_renames.into_iter().filter_map(RawAppRename::into_rename).collect(),
            repo_renames: raw.repo_renames.into_iter().filter_map(RawRepoRename::into_rename).collect(),
            announcements: raw.important_notices.announcements.into_iter().filter_map(|n| n.into_notice("announcement")).take(MAX_NOTICES).collect(),
            alerts: raw.important_notices.alerts.into_iter().filter_map(|n| n.into_notice("alert")).take(MAX_NOTICES).collect(),
            mirror: None,
        };
        // App renames are news too: show them with the announcements, inside the cap.
        feed.announcements.truncate(MAX_NOTICES.saturating_sub(feed.app_renames.len()));
        for r in &feed.app_renames {
            feed.announcements.push(Notice {
                id: format!("rename:{}:{}", r.old_name_lower, r.new_name_lower),
                title: format!("{} is now {}", r.old_name, r.new_name),
                body: r.description.clone(),
                link: None,
                created_at: r.renamed_at,
            });
        }
        feed.announcements.truncate(MAX_NOTICES);
        Some(feed)
    }

    /// Trusts downloads under `base` (which must end in `/`) and keeps every entry fresh: a feed
    /// served by `cargo xtask local serve` on the developer's own machine.
    #[must_use]
    pub fn with_mirror(mut self, base: &str) -> Self {
        self.mirror = base.ends_with('/').then(|| base.to_string());
        self
    }

    /// The repo to ask GitHub about for `app`, following the feed's repo renames. Always under
    /// [`GITHUB_OWNER`]: the feed can move an app within Open3D's account, never out of it.
    pub fn repo_for(&self, app: &SuiteApp) -> String {
        let mut repo = app.repo.to_string();
        // Follow a chain of moves (a → b → c), without looping forever.
        for _ in 0..8 {
            match self.repo_renames.iter().find(|r| r.old_repo.eq_ignore_ascii_case(&repo)) {
                Some(r) if !r.new_repo.eq_ignore_ascii_case(&repo) => repo = r.new_repo.clone(),
                _ => break,
            }
        }
        repo
    }

    /// What the feed says about `app` at `now`.
    pub fn answer(&self, app: &SuiteApp, now: i64) -> FeedAnswer {
        let Some(entry) = self.apps.get(app.id) else { return FeedAnswer::AskGitHub };
        let Some(fetched_at) = entry.fetched_at else { return FeedAnswer::AskGitHub };
        // A timestamp from the future (clock skew) counts as just now; one too old doesn't count.
        // A local mirror is as fresh as its last sync.
        if self.mirror.is_none() && now.saturating_sub(fetched_at) > MAX_AGE {
            return FeedAnswer::AskGitHub;
        }
        // The feed's repo must be the app's, or one the feed says it moved to.
        let repo = self.repo_for(app);
        if let Some(feed_repo) = &entry.repo
            && !feed_repo.eq_ignore_ascii_case(&repo)
        {
            return FeedAnswer::AskGitHub;
        }
        match &entry.release {
            None => FeedAnswer::Fresh(None),
            Some(r) => match r.to_release(&repo, self.mirror.as_deref()) {
                Some(release) => FeedAnswer::Fresh(Some(release)),
                None => FeedAnswer::AskGitHub,
            },
        }
    }
}

impl FeedRelease {
    /// The release with only the downloads that belong to `repo` (or sit under `mirror`). `None`
    /// when it doesn't hold up (no tag, a tag with odd characters, or not a version).
    fn to_release(&self, repo: &str, mirror: Option<&str>) -> Option<Release> {
        let version = version::parse(&self.tag).filter(|_| release::valid_tag(&self.tag))?;
        let allowed = |name: &str, url: &str| {
            (release::download_url_under(url, &release::download_prefix(repo))
                && url == format!("{}{}/{}", release::download_prefix(repo), self.tag, name))
                || mirror.is_some_and(|m| release::download_url_under_mirror(url, m))
        };
        let releases_page = format!("https://github.com/{repo}/releases/");
        let assets = self
            .assets
            .iter()
            .filter(|(name, _, url, _)| release::valid_file_name(name) && allowed(name, url))
            .map(|(name, size, url, sha)| Asset {
                name: name.clone(),
                size: *size,
                url: url.clone(),
                sha256: sha.clone().filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).map(|h| h.to_ascii_lowercase()),
            })
            .collect();
        Some(Release {
            version: Some(version),
            name: if self.name.is_empty() { self.tag.clone() } else { truncate(&self.name, 120) },
            tag: self.tag.clone(),
            published_at: self.published_at,
            html_url: release::safe_release_html_url(repo, &self.html_url)
                .or_else(|| {
                    let fallback = format!("{releases_page}tag/{}", self.tag);
                    release::safe_release_html_url(repo, &fallback)
                })
                .unwrap_or_else(|| format!("https://github.com/{repo}/releases")),
            notes: truncate(&self.notes, 64 * 1024),
            prerelease: self.prerelease,
            assets,
        })
    }
}

// ---- lenient wire format ----------------------------------------------------------------------

/// Reads `T`, or its default when the value is missing, `null` or the wrong shape.
fn lenient<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(T::deserialize(value).unwrap_or_default())
}

/// A list where entries of the wrong shape are dropped rather than spoiling the rest.
fn lenient_vec<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Vec<T>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let items = match value {
        serde_json::Value::Array(items) => items,
        _ => return Ok(Vec::new()),
    };
    Ok(items.into_iter().filter_map(|v| T::deserialize(v).ok()).collect())
}

/// A map where entries of the wrong shape are dropped.
fn lenient_map<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<BTreeMap<String, T>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let entries = match value {
        serde_json::Value::Object(entries) => entries,
        _ => return Ok(BTreeMap::new()),
    };
    Ok(entries.into_iter().filter_map(|(k, v)| T::deserialize(v).ok().map(|t| (k, t))).collect())
}

#[derive(Deserialize, Default)]
struct RawResponse {
    #[serde(default, deserialize_with = "lenient")]
    release_info: RawReleaseInfo,
    #[serde(default, deserialize_with = "lenient_vec")]
    app_renames: Vec<RawAppRename>,
    #[serde(default, deserialize_with = "lenient_vec")]
    repo_renames: Vec<RawRepoRename>,
    #[serde(default, deserialize_with = "lenient")]
    important_notices: RawNotices,
}

#[derive(Deserialize, Default)]
struct RawReleaseInfo {
    #[serde(default, deserialize_with = "lenient_map")]
    apps: BTreeMap<String, RawApp>,
    #[serde(default, deserialize_with = "lenient")]
    last_fetched: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawApp {
    #[serde(default, deserialize_with = "lenient")]
    repo: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    latest_release: Option<RawRelease>,
    #[serde(default, deserialize_with = "lenient")]
    app_last_fetched: Option<String>,
}

impl RawApp {
    fn into_app(self) -> FeedApp {
        FeedApp {
            repo: self.repo.filter(|r| r.split('/').count() == 2 && r.split('/').all(|part| !part.is_empty() && part != "." && part != "..")),
            release: self.latest_release.and_then(RawRelease::into_release),
            fetched_at: self.app_last_fetched.as_deref().and_then(parse_rfc3339),
        }
    }
}

#[derive(Deserialize, Default)]
struct RawRelease {
    #[serde(default, deserialize_with = "lenient")]
    tag: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    published_at: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    html_url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    prerelease: bool,
    #[serde(default, deserialize_with = "lenient")]
    notes_markdown: Option<String>,
    #[serde(default, deserialize_with = "lenient_vec")]
    assets: Vec<RawAsset>,
}

impl RawRelease {
    fn into_release(self) -> Option<FeedRelease> {
        let tag = self.tag.filter(|t| !t.is_empty() && t.len() <= 64)?;
        Some(FeedRelease {
            tag,
            name: self.name.unwrap_or_default(),
            published_at: self.published_at.as_deref().and_then(parse_rfc3339),
            html_url: self.html_url.unwrap_or_default(),
            prerelease: self.prerelease,
            notes: self.notes_markdown.unwrap_or_default(),
            assets: self.assets.into_iter().filter_map(|a| Some((a.name?, a.size_bytes, a.download_url?, a.sha256))).take(300).collect(),
        })
    }
}

#[derive(Deserialize, Default)]
struct RawAsset {
    #[serde(default, deserialize_with = "lenient")]
    name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    size_bytes: u64,
    #[serde(default, deserialize_with = "lenient")]
    download_url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    sha256: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawAppRename {
    #[serde(default, deserialize_with = "lenient")]
    old_name: String,
    #[serde(default, deserialize_with = "lenient")]
    old_name_lower: String,
    #[serde(default, deserialize_with = "lenient")]
    new_name: String,
    #[serde(default, deserialize_with = "lenient")]
    new_name_lower: String,
    #[serde(default, deserialize_with = "lenient")]
    description: String,
    #[serde(default, deserialize_with = "lenient")]
    renamed_at: Option<String>,
}

impl RawAppRename {
    fn into_rename(self) -> Option<AppRename> {
        (!self.old_name_lower.is_empty() && !self.new_name_lower.is_empty()).then(|| AppRename {
            old_name: if self.old_name.is_empty() { self.old_name_lower.clone() } else { truncate(&self.old_name, 80) },
            new_name: if self.new_name.is_empty() { self.new_name_lower.clone() } else { truncate(&self.new_name, 80) },
            old_name_lower: self.old_name_lower.to_lowercase(),
            new_name_lower: self.new_name_lower.to_lowercase(),
            description: truncate(&self.description, MAX_TEXT),
            renamed_at: self.renamed_at.as_deref().and_then(parse_rfc3339),
        })
    }
}

#[derive(Deserialize, Default)]
struct RawRepoRename {
    #[serde(default, deserialize_with = "lenient")]
    old_name: String,
    #[serde(default, deserialize_with = "lenient")]
    new_name: String,
    #[serde(default, deserialize_with = "lenient")]
    renamed_at: Option<String>,
}

impl RawRepoRename {
    /// `None` unless both names are `owner/name` under [`GITHUB_OWNER`]. A feed that could send
    /// an app's downloads to someone else's repository would make its checksums worthless (#90).
    fn into_rename(self) -> Option<RepoRename> {
        let ok = |r: &str| {
            let mut parts = r.split('/');
            matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if a.eq_ignore_ascii_case(GITHUB_OWNER) && !b.is_empty() && b != "." && b != ".."
          && r.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_./".contains(&c)))
        };
        (ok(&self.old_name) && ok(&self.new_name)).then(|| RepoRename {
            old_repo: self.old_name,
            new_repo: self.new_name,
            renamed_at: self.renamed_at.as_deref().and_then(parse_rfc3339),
        })
    }
}

#[derive(Deserialize, Default)]
struct RawNotices {
    #[serde(default, deserialize_with = "lenient_vec")]
    announcements: Vec<RawNotice>,
    #[serde(default, deserialize_with = "lenient_vec")]
    alerts: Vec<RawNotice>,
}

#[derive(Deserialize, Default)]
struct RawNotice {
    #[serde(default, deserialize_with = "lenient")]
    title: String,
    #[serde(default, deserialize_with = "lenient")]
    body: String,
    #[serde(default, deserialize_with = "lenient")]
    maybe_web_link: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    created_at: Option<String>,
}

impl RawNotice {
    fn into_notice(self, kind: &str) -> Option<Notice> {
        if self.title.trim().is_empty() && self.body.trim().is_empty() {
            return None;
        }
        let link = self.maybe_web_link.filter(|l| release::safe_web_link(l));
        Some(Notice {
            id: format!("{kind}:{}:{}", self.created_at.as_deref().unwrap_or(""), self.title.trim()),
            title: truncate(self.title.trim(), 200),
            body: truncate(self.body.trim(), MAX_TEXT),
            link,
            created_at: self.created_at.as_deref().and_then(parse_rfc3339),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-10-09T…
    const HASH: &str = "8a5edab282632443219e051e4ade2d1d5bbc671c781051bf1437897cbdfea0f1";

    /// A feed answer, abridged.
    fn sample() -> String {
        format!(
            r#"{{
        "success": true,
        "release_info": {{
          "apps": {{
            "phototool": {{
              "name": "PhotoTool", "repo": "carlesllavata/phototool",
              "latest_release": {{
                "tag": "v0.6.0", "version": "0.6.0", "name": "PhotoTool v0.6.0",
                "published_at": "2026-10-09T10:00:00Z",
                "html_url": "https://github.com/carlesllavata/phototool/releases/tag/v0.6.0",
                "prerelease": false, "notes_markdown": "Notes",
                "assets": [
                  {{ "name": "phototool-0.6.0-macos-universal.dmg", "size_bytes": 123,
                     "download_url": "https://github.com/carlesllavata/phototool/releases/download/v0.6.0/phototool-0.6.0-macos-universal.dmg",
                     "sha256": "{HASH}", "platform": {{ "os": "macos", "arch": "universal", "package_type": "dmg" }} }},
                  {{ "name": "evil.dmg", "size_bytes": 1, "download_url": "https://evil.example/evil.dmg", "sha256": null }}
                ]
              }},
              "app_last_fetched": "2026-10-09T13:00:00Z"
            }},
            "soundtool": {{ "name": "SoundTool", "repo": "carlesllavata/soundtool", "latest_release": null,
                             "app_last_fetched": "2026-10-09T13:00:00Z" }},
            "filmtool": {{ "name": "FilmTool", "repo": "carlesllavata/filmtool", "latest_release": null, "app_last_fetched": null }},
            "vectortool": {{ "repo": "carlesllavata/vectortool", "app_last_fetched": "2026-10-01T00:00:00Z",
                              "latest_release": {{ "tag": "v0.7.0" }} }},
            "brandnewtool": {{ "repo": "carlesllavata/brandnewtool", "app_last_fetched": "2026-10-09T13:00:00Z" }}
          }},
          "last_fetched": "2026-10-09T13:00:00Z",
          "some_future_field": {{ "whatever": [1, 2, 3] }}
        }},
        "app_renames": [],
        "repo_renames": [],
        "important_notices": {{ "announcements": [], "alerts": [] }}
      }}"#
        )
    }

    #[test]
    fn rename_notices_count_toward_the_cap() {
        let rename = |i: usize| {
            format!(
                r#"{{"old_name": "PdfTool{i}", "old_name_lower": "pdftool{i}", "new_name": "DocuTool{i}", "new_name_lower": "docutool{i}",
            "description": "Same app, new name.", "renamed_at": "2026-10-09T08:00:00Z"}}"#
            )
        };
        for (news, renames) in [(MAX_NOTICES, 1), (1000, 1), (0, 30)] {
            let many: Vec<String> = (0..news).map(|i| format!(r#"{{"title": "News {i}", "body": "x"}}"#)).collect();
            let moved: Vec<String> = (0..renames).map(rename).collect();
            let full = sample()
                .replace("\"announcements\": []", &format!("\"announcements\": [{}]", many.join(",")))
                .replace("\"app_renames\": []", &format!("\"app_renames\": [{}]", moved.join(",")));
            let notices = Feed::parse(&full).unwrap().announcements;
            assert_eq!(notices.len(), MAX_NOTICES, "{news} announcements, {renames} renames");
            assert!(notices.iter().any(|n| n.title == "PdfTool0 is now DocuTool0"), "{news} announcements: the rename was cut");
        }
    }
}
