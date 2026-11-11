// SPDX-License-Identifier: GPL-3.0-or-later
//! [`NativeBackend`]: the desktop implementation of the UI's [`Backend`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use open3d_launcher_core::catalog::{LAUNCHER, LAUNCHER_ID};
use open3d_launcher_core::feed::{Feed, FeedAnswer};
use open3d_launcher_core::platform::{Arch, Os, Platform, pick_asset};
use open3d_launcher_core::protocol::{
    Backend, BackendOptions, Capabilities, Command, Event, JobPhase, LatestRelease, Origin, PAUSED, PausedDownload, Source, VersionTrack,
};
use open3d_launcher_core::release::{Asset, Release};
use open3d_launcher_core::{Installed, PackageKind, SuiteApp, version};

use crate::feed_throttle::{self, FeedThrottle, Outcome};
use crate::http::{self, Http, Stop};
use crate::install::{self, Job};
use crate::manifest::Manifest;
use crate::paths::Paths;
use crate::source::{GitHubSource, ReleaseSource};

/// The longest the launcher's own update waits for other installs to end before it gives up
/// (it can be cancelled sooner).
const RESTART_WAIT: Duration = Duration::from_secs(10 * 60);
/// Automatic checks closer together than this reuse the last answers. The UI asks every five
/// minutes; this stops a burst of requests (refresh, focus, timers) from repeating work.
const FRESH_FOR: i64 = 4 * 60;
/// Once every app has loaded, GitHub is asked about an app the feed can't vouch for at most this
/// often (pressing Refresh asks right away).
const GITHUB_FALLBACK_EVERY: i64 = 60 * 60;
/// Until every app has loaded once, GitHub may be asked a little more eagerly.
const EARLY_GITHUB_EVERY: i64 = 5 * 60;
/// Tries at the feed per check (after the very first poll). The pause between tries is the
/// [`FeedThrottle`]'s: seconds during the first load, so a check retries; a minute or more after
/// that, so a check then tries once and the retries come from the next checks.
const FEED_ATTEMPTS: usize = 3;
/// The longest a check waits on the [`FeedThrottle`] before carrying on without the feed.
const MAX_THROTTLE_WAIT: i64 = 10;
/// After everything has loaded, the feed counts as seriously failed (and GitHub takes over)
/// only after this many checks in a row without an answer…
const SERIOUS_FEED_FAILURES: u32 = 3;
/// …and this long without a good answer.
const SERIOUS_FEED_OUTAGE: i64 = 60 * 60;
/// Requests in flight at once during a check.
const WORKERS: usize = 4;

type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct DownloadSize {
    revision: u64,
    plan: Option<(PackageKind, u64)>,
}

impl DownloadSize {
    fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.plan = None;
    }
}

struct Shared {
    events: Mutex<Vec<Event>>,
    /// Held while an app's latest release or installed copy changes and its [`Event::Installable`]
    /// is worked out and queued, so no answer from older state lands after a newer one. Always
    /// taken first, before any other lock here.
    reporting: Mutex<()>,
    /// Held while a report of what an app's install would download is worked out and sent
    /// ([`report_partial`]), so one worked out from older settings or state can't land after a newer
    /// one. Taken before `reporting`.
    planning: Mutex<()>,
    /// What this system could install, and when that was asked ([`Shared::system_kinds`]).
    system: Mutex<Option<(Instant, install::SystemKinds)>>,
    waker: Waker,
    latest: Mutex<HashMap<String, Release>>,
    download_sizes: Mutex<HashMap<String, DownloadSize>>,
    installed: Mutex<HashMap<String, Installed>>,
    jobs: Mutex<HashMap<String, Arc<Stop>>>,
    /// The launcher is replacing itself and will restart: no new job starts.
    restarting: AtomicBool,
    manifest: Mutex<Manifest>,
    options: Mutex<BackendOptions>,
    checking: AtomicBool,
    /// A detection pass is running; another request waits for the next one.
    detecting: AtomicBool,
    last_check: Mutex<Option<i64>>,
    /// The newer Open3D Launcher release, once a check has found one.
    launcher_release: Mutex<Option<Release>>,
    /// How this launcher is installed, when it can replace itself. Worked out on the check thread
    /// just before a newer launcher is announced: finding out can ask the host's Flatpak, which
    /// mustn't hold up the first frame.
    self_install: OnceLock<Option<crate::self_update::Install>>,
    /// When GitHub was last asked about each app (Unix seconds), to keep the fallback rare.
    github_asked: Mutex<HashMap<String, i64>>,
    /// Apps with an answer (a release or "none yet") from either service.
    answered: Mutex<std::collections::HashSet<String>>,
    /// Every app has had an answer at least once: from then on GitHub is asked sparingly.
    loaded: AtomicBool,
    /// Checks in a row in which the feed didn't answer.
    feed_failures: Mutex<u32>,
    /// When the feed last answered (Unix seconds).
    feed_ok_at: Mutex<Option<i64>>,
    /// How often Open3D's API may be called.
    feed_throttle: Mutex<FeedThrottle>,
    /// Apps whose details pictures are on their way (a second request meanwhile does nothing).
    pictures_busy: Mutex<std::collections::HashSet<&'static str>>,
}

#[derive(Clone, Copy)]
enum BusyFlag {
    Checking,
    Detecting,
}

struct BusyGuard {
    shared: Arc<Shared>,
    flag: BusyFlag,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        let flag = match self.flag {
            BusyFlag::Checking => &self.shared.checking,
            BusyFlag::Detecting => &self.shared.detecting,
        };
        flag.store(false, Ordering::SeqCst);
    }
}

struct JobGuard {
    shared: Arc<Shared>,
    app_id: String,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.app_id);
    }
}

/// How long the installability answer trusts what this system could install ([`Shared::system_kinds`]).
const SYSTEM_KINDS_FOR: Duration = Duration::from_secs(60);

impl Shared {
    fn store_latest(&self, app: &str, release: &Release) {
        let mut latest = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
        if latest.get(app) != Some(release) {
            latest.insert(app.to_string(), release.clone());
            self.invalidate_download_size(app);
        }
    }

    fn store_detected(&self, app: &str, found: Option<&Installed>) {
        let mut installed = self.installed.lock().unwrap_or_else(PoisonError::into_inner);
        if installed.get(app) != found {
            match found {
                Some(i) => installed.insert(app.to_string(), i.clone()),
                None => installed.remove(app),
            };
            self.invalidate_download_size(app);
        }
    }

    fn invalidate_download_size(&self, app: &str) {
        self.download_sizes.lock().unwrap_or_else(PoisonError::into_inner).entry(app.to_string()).or_default().invalidate();
    }

    fn download_revision(&self, app: &str) -> u64 {
        self.download_sizes.lock().unwrap_or_else(PoisonError::into_inner).entry(app.to_string()).or_default().revision
    }

    fn report_download(&self, app: &str, revision: u64, plan: Option<(PackageKind, u64)>, download: Option<PausedDownload>, ready: bool) -> bool {
        {
            let mut sizes = self.download_sizes.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(cached) = sizes.get_mut(app).filter(|cached| cached.revision == revision) else { return false };
            cached.plan = plan;
            let mut events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
            events.push(Event::Paused { app: app.to_string(), download });
            events.push(Event::ReadyToInstall { app: app.to_string(), ready });
        }
        (self.waker)();
        true
    }

    /// What this system can install, asked again at most once a [`SYSTEM_KINDS_FOR`]: inside the
    /// Flatpak sandbox asking runs a program on the host (up to 3 s), and the installability answer
    /// is worked out for each installed app on every check. An install still asks afresh. Always
    /// asked before `reporting` is taken. [`report_partial`] asks it holding `planning`, which only
    /// its own worker threads take.
    fn system_kinds(&self) -> install::SystemKinds {
        if let Some((at, kinds)) = self.system.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
            && at.elapsed() < SYSTEM_KINDS_FOR
        {
            return kinds.clone();
        }
        let kinds = install::SystemKinds::now();
        *self.system.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), kinds.clone()));
        kinds
    }

    /// What this system could install when last asked, however long ago, for an answer worked out
    /// holding `reporting`: no probe runs there. A copy only enters `installed` through
    /// [`publish_detected`], which asks [`Self::system_kinds`] first, so with a copy installed
    /// there is always a view; without one (never in practice) nothing installs.
    fn last_system_kinds(&self) -> install::SystemKinds {
        self.system
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or_else(|| install::SystemKinds { available: Vec::new(), can_pacman: false }, |(_, kinds)| kinds.clone())
    }

    /// Whether some copy of `app` is installed (as last detected).
    fn has_copy(&self, app: &SuiteApp) -> bool {
        self.installed.lock().unwrap_or_else(PoisonError::into_inner).contains_key(app.id)
    }

    fn emit(&self, event: Event) {
        self.emit_all([event]);
    }

    /// Queues `events` together: a poll sees all of them or none.
    fn emit_all(&self, events: impl IntoIterator<Item = Event>) {
        self.events.lock().unwrap_or_else(PoisonError::into_inner).extend(events);
        (self.waker)();
    }
}

pub struct NativeBackend {
    shared: Arc<Shared>,
    paths: Paths,
    http: Option<Http>,
    source: Option<Arc<dyn ReleaseSource>>,
    platform: Option<Platform>,
    /// The version-choice file the apps read (see [`crate::pins`]); `None` when there is no
    /// per-user data folder.
    pins: Option<std::path::PathBuf>,
}

impl NativeBackend {
    /// `waker` is called (from any thread) whenever events are waiting; it should repaint the UI.
    pub fn new(waker: impl Fn() + Send + Sync + 'static) -> Self {
        let waker: Waker = Arc::new(waker);
        let paths = Paths::new().unwrap_or_else(|e| {
            log::error!("{e}; keeping files in the temporary folder");
            Paths::under(&std::env::temp_dir().join("open3d-launcher"))
        });
        let http = match Http::new() {
            Ok(h) => Some(h),
            Err(e) => {
                log::error!("HTTPS unavailable: {e}");
                None
            }
        };
        let source: Option<Arc<dyn ReleaseSource>> =
            http.clone().map(|h| Arc::new(GitHubSource::new(h, paths.release_cache())) as Arc<dyn ReleaseSource>);
        let shared = Arc::new(Shared {
            events: Mutex::new(Vec::new()),
            reporting: Mutex::new(()),
            system: Mutex::new(None),
            planning: Mutex::new(()),
            waker,
            latest: Mutex::new(HashMap::new()),
            download_sizes: Mutex::new(HashMap::new()),
            installed: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            restarting: AtomicBool::new(false),
            manifest: Mutex::new(Manifest::load(&paths.manifest())),
            options: Mutex::new(BackendOptions::default()),
            checking: AtomicBool::new(false),
            detecting: AtomicBool::new(false),
            launcher_release: Mutex::new(None),
            self_install: OnceLock::new(),
            github_asked: Mutex::new(HashMap::new()),
            answered: Mutex::new(std::collections::HashSet::new()),
            loaded: AtomicBool::new(false),
            feed_failures: Mutex::new(0),
            feed_ok_at: Mutex::new(None),
            feed_throttle: Mutex::new(FeedThrottle::load(&paths.feed_throttle(), crate::now())),
            pictures_busy: Mutex::new(std::collections::HashSet::new()),
            last_check: Mutex::new(None),
        });
        let pins = craft_update::pins::Pins::default_path();
        if let Some(path) = &pins {
            crate::pins::prepare_folder(path);
        }
        let backend = Self { shared, paths, http, source, platform: native_platform(), pins };
        backend.replay_cache();
        backend.detect_all();
        backend
    }

    /// Shows the saved answers from the last run straight away, before any network request: the
    /// release feed's where they're still fresh, GitHub's otherwise.
    fn replay_cache(&self) {
        let options = self.options();
        let pre = options.prereleases;
        let feed = if pre { None } else { load_feed(&self.paths) };
        let now = crate::now();
        for app in open3d_launcher_core::hosted_catalog::apps().iter() {
            let answer = match feed.as_ref().map(|f| f.answer(app, now)) {
                Some(FeedAnswer::Fresh(Some(r))) => Some((LatestRelease::Found(r), Origin::ReleaseFeed)),
                Some(FeedAnswer::Fresh(None)) => Some((LatestRelease::None, Origin::ReleaseFeed)),
                _ => self.source.as_ref().and_then(|s| s.cached(&feed_repo(feed.as_ref(), app), pre)).map(|l| (l, Origin::GitHub)),
            };
            if let Some((latest, from)) = answer {
                publish_latest(app, latest, from, &self.shared, self.platform);
            }
        }
        if let Some(f) = feed {
            self.shared.emit(Event::Notices { announcements: f.announcements, alerts: f.alerts });
        }
    }

    /// Works out again, off the UI thread, what each app's install would download, and tells the
    /// UI ([`report_partial`]): inside the Flatpak sandbox that asks the host (up to 3 s an app).
    fn replan(&self) {
        let shared = self.shared.clone();
        let paths = self.paths.clone();
        let platform = self.platform;
        std::thread::spawn(move || {
            for app in open3d_launcher_core::hosted_catalog::apps().iter() {
                report_partial(app, &shared, &paths, platform);
            }
        });
    }

    fn options(&self) -> BackendOptions {
        self.shared.options.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Scans for installed Open3D apps and paused downloads on a background thread.
    fn detect_all(&self) {
        if self.shared.detecting.swap(true, Ordering::SeqCst) {
            return;
        }
        let shared = self.shared.clone();
        let busy = BusyGuard { shared: shared.clone(), flag: BusyFlag::Detecting };
        let paths = self.paths.clone();
        let platform = self.platform;
        std::thread::spawn(move || {
            let busy = busy;
            // Leftovers from interrupted installs or older versions of the launcher. A paused
            // download stays a month; a finished one kept after a failed install, a week.
            http::clean_downloads(&paths.downloads(), Duration::from_secs(30 * 86_400), Duration::from_secs(7 * 86_400));
            let manifest = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).clone();
            for app in open3d_launcher_core::hosted_catalog::apps().iter() {
                let found = crate::detect::detect(app, &manifest);
                publish_detected(app, found, &shared, platform);
            }
            // Downloads paused (or cut off by quitting) in an earlier run.
            for app in open3d_launcher_core::hosted_catalog::apps().iter() {
                report_partial(app, &shared, &paths, platform);
            }
            drop(busy);
        });
    }

    /// Looks up the latest releases: Open3D's release feed first (one request for every app),
    /// GitHub for anything the feed can't vouch for (apps it doesn't list, entries more than a
    /// day old, or no answer at all), at most once an hour per app unless `force`.
    fn check(&self, force: bool) {
        let (Some(source), Some(http)) = (self.source.clone(), self.http.clone()) else {
            crate::hosted_catalog::load_once(None, &self.paths);
            self.shared.emit(Event::CatalogUpdated);
            for app in open3d_launcher_core::hosted_catalog::apps().iter() {
                self.shared.emit(Event::Latest {
                    app: app.id.to_string(),
                    latest: LatestRelease::Failed("secure connections aren't available".into()),
                    from: Origin::GitHub,
                });
            }
            return;
        };
        let now = crate::now();
        let recent = self.shared.last_check.lock().unwrap_or_else(PoisonError::into_inner).is_some_and(|t| now - t < FRESH_FOR);
        if (recent && !force) || self.shared.checking.swap(true, Ordering::SeqCst) {
            return;
        }
        let shared = self.shared.clone();
        let busy = BusyGuard { shared: shared.clone(), flag: BusyFlag::Checking };
        let paths = self.paths.clone();
        let platform = self.platform;
        let options = self.options();
        let pre = options.prereleases;
        let mirror = local_mirror();
        std::thread::spawn(move || {
            let busy = busy;
            shared.emit(Event::CheckStarted);
            crate::hosted_catalog::load_once(Some(&http), &paths);
            shared.emit(Event::CatalogUpdated);
            let (feed, fallback) = check_plan(&http, mirror.as_ref(), pre, &paths, &shared);
            if let Some(f) = &feed {
                shared.emit(Event::Notices { announcements: f.announcements.clone(), alerts: f.alerts.clone() });
            }
            let now = crate::now();
            let queue = Mutex::new(open3d_launcher_core::hosted_catalog::apps().iter().chain(std::iter::once(&LAUNCHER)).collect::<Vec<&SuiteApp>>());
            let sources = sweep(&queue, &mirror, feed.as_ref(), fallback, &source, &shared, &paths, (platform, &options), force, now);
            source.save();
            {
                let answered = shared.answered.lock().unwrap_or_else(PoisonError::into_inner);
                if open3d_launcher_core::hosted_catalog::apps().iter().all(|a| answered.contains(a.id)) {
                    shared.loaded.store(true, Ordering::SeqCst);
                }
            }
            // A newer release changes which file an install would use.
            for app in open3d_launcher_core::hosted_catalog::apps().iter() {
                report_partial(app, &shared, &paths, platform);
            }
            let source_used = source_used_of(&sources);
            let at = crate::now();
            *shared.last_check.lock().unwrap_or_else(PoisonError::into_inner) = Some(at);
            let rate_limited_until = limit_notice(source_used, source.rate_limited_until());
            drop(busy);
            shared.emit(Event::CheckFinished { at, source: source_used, rate_limited_until });
        });
    }

    /// Starts the job, or returns `false` without starting one: the app is unknown, doesn't run
    /// on this platform, is already running a job, or the launcher is restarting into an update.
    fn start_job(&self, app_id: &str, work: Work) -> bool {
        let uninstall = matches!(work, Work::Uninstall);
        let Some(app) = SuiteApp::by_id(app_id) else { return false };
        log::info!("{}: starting {} job", app.name, if uninstall { "uninstall" } else { "install/update" });
        let stop = Arc::new(Stop::default());
        let restarting = {
            let mut jobs = self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner);
            if jobs.contains_key(app.id) {
                return false;
            }
            let restarting = self.shared.restarting.load(Ordering::SeqCst);
            if !restarting {
                jobs.insert(app.id.to_string(), stop.clone());
            }
            restarting
        };
        if restarting {
            let result = Err("Open3D Launcher is restarting to finish its update. Try again once it's back.".into());
            self.shared.emit(Event::JobFinished { app: app.id.to_string(), result });
            return false;
        }
        let shared = self.shared.clone();
        let job = JobGuard { shared: shared.clone(), app_id: app.id.to_string() };
        let paths = self.paths.clone();
        let http = self.http.clone();
        let platform = self.platform;
        let options = self.options();
        let source = self.source.clone();
        std::thread::spawn(move || {
            let job = job;
            let existing = shared.installed.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned();
            let record = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).apps.get(app.id).cloned();
            let result = if uninstall {
                shared.emit(Event::Progress { app: app.id.to_string(), phase: JobPhase::Removing });
                let mut phase = |phase| shared.emit(Event::Progress { app: app.id.to_string(), phase });
                match &existing {
                    Some(inst) => install::uninstall(app, inst, record.as_ref(), &paths, &stop, &mut phase).map(|()| {
                        let mut m = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner);
                        if m.apps.remove(app.id).is_some() {
                            let _ = m.save(&paths.manifest());
                        }
                        format!("{} was removed", app.name)
                    }),
                    None => Err(format!("{} isn't installed", app.name)),
                }
            } else {
                let approved = match &work {
                    Work::Replace(path) => Some(path.as_str()),
                    Work::Install | Work::Uninstall => None,
                };
                let r = run_install(
                    app,
                    &shared,
                    &paths,
                    http.as_ref(),
                    platform,
                    &options,
                    existing.as_ref(),
                    record.as_ref(),
                    &stop,
                    approved,
                    source.as_deref(),
                );
                // Stopped by the connection rather than by the user: say so, and how to go on.
                match r {
                    Err(e) if e == PAUSED && stop.check().is_ok() => {
                        Err("The download paused because the connection keeps dropping. Press Resume to continue where it stopped.".into())
                    }
                    other => other,
                }
            };
            // Look again: the result is whatever is on disk now.
            let manifest = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).clone();
            let found = crate::detect::detect(app, &manifest);
            // The manifest may change the package plan even when detection finds the same copy.
            shared.invalidate_download_size(app.id);
            publish_detected(app, found, &shared, platform);
            report_partial(app, &shared, &paths, platform);
            drop(job);
            shared.emit(Event::JobFinished { app: app.id.to_string(), result });
        });
        true
    }

    /// Starts one install per app (see [`Command::InstallAll`]). Apps unknown or already running a
    /// job are left out.
    fn install_all(&self, apps: &[String]) {
        for id in apps {
            if SuiteApp::by_id(id).is_some() {
                self.start_job(id, Work::Install);
            }
        }
    }

    /// Downloads the newer launcher, puts it in place and asks the UI to quit so it can start.
    fn update_launcher(&self) {
        let id = LAUNCHER_ID.to_string();
        let (Some(install), Some(platform)) = (self.shared.self_install.get().cloned().flatten(), self.platform) else {
            self.shared.emit(Event::JobFinished {
                app: id,
                result: Err("this copy of Open3D Launcher can't update itself; download the new version instead".into()),
            });
            return;
        };
        let Some(release) = self.shared.launcher_release.lock().unwrap_or_else(PoisonError::into_inner).clone() else { return };
        let stop = Arc::new(Stop::default());
        {
            let mut jobs = self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner);
            if jobs.contains_key(&id) {
                return;
            }
            jobs.insert(id.clone(), stop.clone());
        }
        let job = JobGuard { shared: self.shared.clone(), app_id: id.clone() };
        let (shared, paths, http) = (self.shared.clone(), self.paths.clone(), self.http.clone());
        std::thread::spawn(move || {
            let job = job;
            let mut last_emit = Instant::now() - Duration::from_secs(1);
            let mut phase = |p: JobPhase| {
                if matches!(p, JobPhase::Downloading { done, total } if done != total && done != 0) && last_emit.elapsed() < Duration::from_millis(50)
                {
                    return;
                }
                last_emit = Instant::now();
                shared.emit(Event::Progress { app: LAUNCHER_ID.to_string(), phase: p });
            };
            let last = || {
                let mut waiting = || shared.emit(Event::Progress { app: LAUNCHER_ID.to_string(), phase: JobPhase::Waiting });
                claim_restart(&shared.jobs, &shared.restarting, &stop, RESTART_WAIT, &mut waiting)
            };
            let result = match http.as_ref() {
                Some(http) => crate::self_update::update(&install, &release, platform, http, &paths, &stop, &mut phase, &last),
                None => Err("secure connections aren't available on this system".into()),
            };
            drop(job);
            match result {
                Ok(()) => shared.emit(Event::LauncherRestarting),
                Err(e) => {
                    shared.restarting.store(false, Ordering::SeqCst);
                    shared.emit(Event::JobFinished { app: LAUNCHER_ID.to_string(), result: Err(e) });
                }
            }
        });
    }

    fn installed(&self, app: &str) -> Option<Installed> {
        self.shared.installed.lock().unwrap_or_else(PoisonError::into_inner).get(app).cloned()
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Work {
    /// Install or update.
    Install,
    /// Install over the path the user agreed to overwrite (see [`Command::Replace`]).
    Replace(String),
    Uninstall,
}

fn release_matches_track(release: &Release, track: &VersionTrack, prereleases: bool) -> bool {
    match track {
        VersionTrack::Stable => prereleases || (!release.prerelease && release.version.as_ref().is_some_and(version::is_stable)),
        VersionTrack::Nightly => open3d_launcher_core::release::is_nightly(release),
        VersionTrack::Pinned(tag) => &release.tag == tag,
    }
}

/// Whether `chosen` is a version picked by name that is older than the installed copy.
fn is_chosen_downgrade(track: &VersionTrack, chosen: &Release, existing: Option<&Installed>) -> bool {
    let VersionTrack::Pinned(tag) = track else { return false };
    let (Some(chosen_v), Some(installed_v)) = (chosen.version.as_ref(), existing.and_then(|i| i.version.as_ref())) else { return false };
    chosen.tag == *tag && version::is_newer(installed_v, chosen_v)
}

/// Who asks [`install_plan`].
#[derive(Clone, Copy, Debug)]
enum Asking<'a> {
    /// An install: it asks this system what it can install now, and explains a refusal in full.
    Install,
    /// The installability answer: what this system could install a moment ago
    /// ([`Shared::system_kinds`]), and only whether a refusal would come.
    Answer(&'a install::SystemKinds),
}

/// What an install of `app` would download, worked out from what is installed and chosen
/// before anything is fetched.
struct InstallPlan {
    release: Release,
    asset: Asset,
    kind: PackageKind,
}

/// The checks an install makes before it downloads anything: a copy installed outside the
/// launcher (a system binary, a repository package) isn't replaced; the release must be known and
/// on the chosen version track; and it must have a package that can replace the copy the way it
/// was installed. [`run_install`] and the UI's [`Event::Installable`] both ask this, so the UI
/// never offers an update the install would refuse.
fn install_plan(
    app: &SuiteApp,
    latest: Option<Release>,
    platform: Option<Platform>,
    options: &BackendOptions,
    (existing, record): (Option<&Installed>, Option<&crate::manifest::Record>),
    asking: Asking<'_>,
) -> Result<InstallPlan, String> {
    let platform = platform.ok_or("this system isn't supported")?;
    let release = latest.ok_or("the selected release hasn't been found yet. Check for updates first.")?;
    let track = options.versions.get(app.id).unwrap_or(&VersionTrack::Stable);
    if !release_matches_track(&release, track, options.prereleases) {
        return Err("the selected version isn't available yet. Check for updates before installing.".into());
    }
    let system = match asking {
        Asking::Install => None,
        Asking::Answer(system) => Some(system),
    };
    let (asset, kind) = planned_asset_on(app, &release, platform, options, (existing, record), system)
        .ok_or_else(|| format!("{} {} has no download for {}", app.name, release.version_label(), platform.label()))?;
    Ok(InstallPlan { release, asset, kind })
}

#[allow(clippy::too_many_arguments)]
fn run_install(
    app: &SuiteApp,
    shared: &Shared,
    paths: &Paths,
    http: Option<&Http>,
    platform: Option<Platform>,
    options: &BackendOptions,
    existing: Option<&Installed>,
    record: Option<&crate::manifest::Record>,
    stop: &Stop,
    approved: Option<&str>,
    _source: Option<&dyn ReleaseSource>,
) -> Result<String, String> {
    let latest = shared.latest.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned();
    let InstallPlan { release, asset, kind } = install_plan(app, latest, platform, options, (existing, record), Asking::Install)?;
    let http = http.ok_or("secure connections aren't available on this system")?;
    let track = options.versions.get(app.id).unwrap_or(&VersionTrack::Stable);
    // Going back to a version chosen by name: the 3ds Max bundle keeps the old copy until the new
    // one is in place, so a downgrade that fails leaves it as it was.
    let downgrade = is_chosen_downgrade(track, &release, existing).then_some(install::Downgrade::KeepOldCopy);
    let occupied = install::occupied(app, &asset, kind, existing, record, &options.install_locations, paths)?;
    let replace = match replace_consent(occupied.as_deref(), approved) {
        Consent::Free => false,
        Consent::Given => true,
        Consent::Ask(path) => {
            shared.emit(Event::Occupied { app: app.id.to_string(), path: path.display().to_string() });
            return Err(format!("{} already exists", path.display()));
        }
    };
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let mut phase = |p: JobPhase| {
        // Downloads report thousands of times; the UI needs ~20 a second.
        if matches!(p, JobPhase::Downloading { done, total } if done != total && done != 0) && last_emit.elapsed() < Duration::from_millis(50) {
            return;
        }
        last_emit = Instant::now();
        shared.emit(Event::Progress { app: app.id.to_string(), phase: p });
    };
    let mut job = Job { http, paths, stop, phase: &mut phase, install_locations: &options.install_locations, replace, downgrade };
    let mut commit_record = |record: &crate::manifest::Record| {
        let mut manifest = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner);
        let mut updated = manifest.clone();
        updated.apps.insert(app.id.to_string(), record.clone());
        updated.save(&paths.manifest())?;
        *manifest = updated;
        Ok(())
    };
    let new_record = install::install_at(app, &release, &asset, kind, existing, record, &mut job, Some(&mut commit_record))?;
    {
        let mut m = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner);
        let changed = match new_record {
            Some(r) => {
                m.apps.insert(app.id.to_string(), r);
                true
            }
            // Installed the platform's own way: any old launcher record is stale.
            None => m.apps.remove(app.id).is_some(),
        };
        if changed && let Err(e) = m.save(&paths.manifest()) {
            log::warn!("couldn't save the install record: {e}");
        }
    }
    Ok(installed_text(app, existing.is_some(), &release))
}

/// Whether an install may write over what is at its destination.
#[derive(Debug, PartialEq, Eq)]
enum Consent<'a> {
    /// Nothing the launcher didn't put there is in the way.
    Free,
    /// The user agreed to overwrite exactly this destination.
    Given,
    /// Something is in the way and the user hasn't agreed to overwrite it: ask.
    Ask(&'a std::path::Path),
}

/// The user's agreement to replace a file covers the path they were shown (`approved`), and only
/// that one. The release can change between the question and the answer (a new version, another
/// asset, another folder), and the install then writes somewhere else: it must not take the old
/// "yes" as leave to overwrite a different file (CWE-367).
fn replace_consent<'a>(occupied: Option<&'a std::path::Path>, approved: Option<&str>) -> Consent<'a> {
    match occupied {
        None => Consent::Free,
        Some(path) if approved == Some(path.display().to_string().as_str()) => Consent::Given,
        Some(path) => Consent::Ask(path),
    }
}

/// What a finished install tells the user.
fn installed_text(app: &SuiteApp, update: bool, release: &Release) -> String {
    let verb = if update { "updated to" } else { "installed:" };
    format!("{} {verb} {}", app.name, release.version_label())
}

/// For the launcher's own update, before it replaces itself: waits (saying so through `waiting`)
/// until every other job has ended, then sets `restarting`, which keeps new jobs from starting.
/// The restart that follows would otherwise cut their installs short. The user can cancel the
/// wait (`stop`); after `limit` it gives up and names the apps still busy, since one may be
/// stuck behind a password prompt nobody has answered.
fn claim_restart(
    jobs: &Mutex<HashMap<String, Arc<Stop>>>,
    restarting: &AtomicBool,
    stop: &Stop,
    limit: Duration,
    waiting: &mut dyn FnMut(),
) -> Result<(), String> {
    let started = Instant::now();
    let mut told = false;
    loop {
        stop.check()?;
        let busy: Vec<&'static str> = {
            let jobs = jobs.lock().unwrap_or_else(PoisonError::into_inner);
            let busy = jobs.keys().filter(|id| *id != LAUNCHER_ID).map(|id| SuiteApp::by_id(id).map_or("another app", |a| a.name)).collect();
            if jobs.keys().all(|id| id == LAUNCHER_ID) {
                restarting.store(true, Ordering::SeqCst);
                return Ok(());
            }
            busy
        };
        if started.elapsed() >= limit {
            return Err(restart_blocked(busy));
        }
        if !told {
            told = true;
            waiting();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Why the launcher didn't restart: `busy` apps were still installing, and what to do about it.
fn restart_blocked(mut busy: Vec<&str>) -> String {
    busy.sort_unstable();
    busy.dedup();
    let (names, were, them) = match busy.as_slice() {
        [one] => ((*one).to_string(), "was", "it"),
        _ => (busy.join(", "), "were", "them"),
    };
    format!(
        "Open3D Launcher didn't restart to update: {names} {were} still installing. Answer or close any password prompt, \
         let {them} finish or cancel {them}, then press Update and restart again."
    )
}

/// The file an install of `app` would download now: same format as the installed copy, else
/// the user's preference, else the best this system supports.
fn planned_asset(
    app: &SuiteApp,
    release: &Release,
    platform: Platform,
    options: &BackendOptions,
    existing: Option<&Installed>,
    record: Option<&crate::manifest::Record>,
) -> Option<(Asset, PackageKind)> {
    planned_asset_on(app, release, platform, options, (existing, record), None)
}

/// [`planned_asset`], on what `system` can install when given (else asking this system now).
fn planned_asset_on(
    app: &SuiteApp,
    release: &Release,
    platform: Platform,
    options: &BackendOptions,
    (existing, record): (Option<&Installed>, Option<&crate::manifest::Record>),
    system: Option<&install::SystemKinds>,
) -> Option<(Asset, PackageKind)> {
    let stems: Vec<&str> = app.stems().collect();
    let kinds = if existing.is_none() && record.is_some_and(|r| r.kind == PackageKind::PortableZip) {
        vec![PackageKind::PortableZip]
    } else {
        match system {
            Some(system) => install::choose_kinds_on(app, existing, record, &options.kinds, system),
            None => install::choose_kinds(app, existing, record, &options.kinds),
        }
    };
    pick_asset(release, &stems, platform, &kinds).map(|(a, k)| (a.clone(), k))
}

/// Tells the UI whether installing `app` would go ahead now ([`Event::Installable`]): the same
/// [`install_plan`] an install makes, for the copy that is installed. Nothing is said until its
/// release is known, or while nothing is installed (the UI only uses it for an installed copy).
fn report_installable(app: &SuiteApp, shared: &Shared, platform: Option<Platform>) {
    let system = shared.has_copy(app).then(|| shared.system_kinds());
    let _reporting = shared.reporting.lock().unwrap_or_else(PoisonError::into_inner);
    shared.emit_all(installable_event(app, shared, platform, system));
}

/// [`report_installable`]'s answer from the shared state as it is, with the current settings:
/// whether [`install_plan`] would let an install go ahead. `system` is what this system can
/// install, asked before `reporting` was taken; when a copy appeared since, the view its detection
/// asked for is used, so nothing is probed here. The caller holds `reporting`.
fn installable_event(app: &SuiteApp, shared: &Shared, platform: Option<Platform>, system: Option<install::SystemKinds>) -> Option<Event> {
    let existing = shared.installed.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned()?;
    let release = shared.latest.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned()?;
    let record = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).apps.get(app.id).cloned();
    let options = shared.options.lock().unwrap_or_else(PoisonError::into_inner).clone();
    // A copy appeared after the caller looked: what was asked moments ago, not a probe here.
    let system = system.unwrap_or_else(|| shared.last_system_kinds());
    let installable = install_plan(app, Some(release), platform, &options, (Some(&existing), record.as_ref()), Asking::Answer(&system)).is_ok();
    Some(Event::Installable { app: app.id.to_string(), installable })
}

/// Records `app`'s latest release and tells the UI, with whether an install would go ahead with
/// it, in one batch: the UI never sees the release without its answer.
fn publish_latest(app: &SuiteApp, latest: LatestRelease, from: Origin, shared: &Shared, platform: Option<Platform>) {
    let system = shared.has_copy(app).then(|| shared.system_kinds());
    let _reporting = shared.reporting.lock().unwrap_or_else(PoisonError::into_inner);
    if let LatestRelease::Found(r) = &latest {
        shared.store_latest(app.id, r);
    }
    let installable = installable_event(app, shared, platform, system);
    shared.emit_all(installable.into_iter().chain([Event::Latest { app: app.id.to_string(), latest, from }]));
}

/// Records what is installed of `app` (`found`) and tells the UI, with whether an install would
/// go ahead to replace it and any other copies 3ds Max would see, in one batch.
fn publish_detected(app: &SuiteApp, found: Option<Installed>, shared: &Shared, platform: Option<Platform>) {
    let system = found.is_some().then(|| shared.system_kinds());
    let other_copies = crate::detect::other_copies(app);
    let _reporting = shared.reporting.lock().unwrap_or_else(PoisonError::into_inner);
    shared.store_detected(app.id, found.as_ref());
    let installable = installable_event(app, shared, platform, system);
    shared.emit_all(installable.into_iter().chain([Event::Detected { app: app.id.to_string(), installed: found, other_copies }]));
}

/// Tells the UI whether `app` has a paused download waiting, or a finished one to install, and
/// whether an install would go ahead at all (the package choices may have changed). Worked out
/// with the settings and state as they are now, one report at a time (`planning`), so a worker
/// that started before a settings change never has the last word.
fn report_partial(app: &SuiteApp, shared: &Shared, paths: &Paths, platform: Option<Platform>) {
    let _one_at_a_time = shared.planning.lock().unwrap_or_else(PoisonError::into_inner);
    // A release or detection can change during planning; retry any rejected report.
    loop {
        // Capture the revision before the options: Configure invalidates it while changing options.
        let revision = shared.download_revision(app.id);
        let options = shared.options.lock().unwrap_or_else(PoisonError::into_inner).clone();
        report_installable(app, shared, platform);
        let planned = planned_download(app, shared, paths, platform, &options);
        let download = planned.as_ref().and_then(|(dest, _, size)| Some(PausedDownload { done: http::partial_bytes(dest, *size)?, total: *size }));
        let ready = planned.as_ref().is_some_and(|(dest, _, size)| http::finished(dest, *size));
        if shared.report_download(app.id, revision, planned.as_ref().map(|(_, kind, size)| (*kind, *size)), download, ready) {
            return;
        }
    }
}

/// Where the planned download of `app` is kept, its package format and published size.
fn planned_download(
    app: &SuiteApp,
    shared: &Shared,
    paths: &Paths,
    platform: Option<Platform>,
    options: &BackendOptions,
) -> Option<(std::path::PathBuf, PackageKind, u64)> {
    let release = shared.latest.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned()?;
    let existing = shared.installed.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned();
    let record = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).apps.get(app.id).cloned();
    let (asset, kind) = planned_asset(app, &release, platform?, options, existing.as_ref(), record.as_ref())?;
    open3d_launcher_core::release::valid_file_name(&asset.name).then(|| (install::download_path(paths, &asset), kind, asset.size))
}

/// What one check asks: which feed to poll (a local mirror trumps everything), and how much to
/// lean on GitHub.
fn check_plan(http: &Http, mirror: Option<&String>, pre: bool, paths: &Paths, shared: &Arc<Shared>) -> (Option<Feed>, Fallback) {
    if let Some(base) = mirror {
        // A local mirror answers for everything: GitHub is never asked.
        (local_feed(http, base), Fallback { trust_feed: true, github_every: GITHUB_FALLBACK_EVERY })
    } else if pre {
        // The feed serves stable releases only; with pre-releases on, GitHub decides.
        (None, Fallback { trust_feed: false, github_every: EARLY_GITHUB_EVERY })
    } else {
        feed_and_fallback(http, paths, shared)
    }
}

/// Answers the apps from a shared queue over a few workers, and returns which sources answered.
#[allow(clippy::too_many_arguments)]
fn sweep(
    queue: &Mutex<Vec<&'static SuiteApp>>,
    mirror: &Option<String>,
    feed: Option<&Feed>,
    fallback: Fallback,
    source: &Arc<dyn ReleaseSource>,
    shared: &Arc<Shared>,
    paths: &Paths,
    (platform, options): (Option<Platform>, &BackendOptions),
    force: bool,
    now: i64,
) -> Vec<Source> {
    let sources = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            scope.spawn(|| {
                loop {
                    let Some(app) = queue.lock().unwrap_or_else(PoisonError::into_inner).pop() else { break };
                    let track =
                        if app.id == LAUNCHER_ID { &options.launcher_version } else { options.versions.get(app.id).unwrap_or(&VersionTrack::Stable) };
                    let (latest, from, used) = match mirror {
                        Some(_) => mirror_chosen(app, feed, now, track),
                        None if *track != VersionTrack::Stable => {
                            let answer = source.chosen(&feed_repo(feed, app), track);
                            (Some(answer.latest), Origin::GitHub, Some(answer.source))
                        }
                        None => latest_for(app, feed, fallback, source.as_ref(), shared, options.prereleases, force, now),
                    };
                    if let Some(used) = used {
                        sources.lock().unwrap_or_else(PoisonError::into_inner).push(used);
                    }
                    check_app(app, latest, from, shared, paths, platform);
                }
            });
        }
    });
    sources.into_inner().unwrap_or_else(PoisonError::into_inner)
}

/// Publishes one app's answer to the shared state, or handles the launcher's own update check
/// first (it doesn't appear in the grid, so it has no card of its own to publish to).
fn check_app(app: &'static SuiteApp, latest: Option<LatestRelease>, from: Origin, shared: &Arc<Shared>, paths: &Paths, platform: Option<Platform>) {
    if app.id == LAUNCHER_ID {
        launcher_update(paths, latest, shared);
        return;
    }
    let Some(latest) = latest else { return };
    if !matches!(latest, LatestRelease::Failed(_)) {
        shared.answered.lock().unwrap_or_else(PoisonError::into_inner).insert(app.id.to_string());
    }
    publish_latest(app, latest, from, shared, platform);
}

/// Offers the launcher's own update when its answer is a newer release than the running one.
fn launcher_update(paths: &Paths, latest: Option<LatestRelease>, shared: &Arc<Shared>) {
    let Some(LatestRelease::Found(r)) = latest else { return };
    let (Some(newest), Some(current)) = (r.version.as_ref(), crate::self_update::current_version()) else { return };
    if version::is_newer(newest, &current) && !crate::self_update::just_updated_to(paths, &r) {
        shared.self_install.get_or_init(crate::self_update::current);
        *shared.launcher_release.lock().unwrap_or_else(PoisonError::into_inner) = Some(r.clone());
        shared.emit(Event::LauncherUpdate(r));
    }
}

/// The weakest source that answered this check; it sets the wording of the "checked with" line.
fn source_used_of(sources: &[Source]) -> Source {
    if sources.contains(&Source::Cache) {
        Source::Cache
    } else if sources.contains(&Source::GitHubPages) {
        Source::GitHubPages
    } else if sources.contains(&Source::GitHubApi) {
        Source::GitHubApi
    } else {
        Source::Feed
    }
}

/// How much the feed is trusted in one check, and how freely GitHub may be asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fallback {
    /// Use the feed's answers (fresh entries only). Off when the feed has seriously failed.
    trust_feed: bool,
    /// Ask GitHub about an app at most this often (seconds), unless forced.
    github_every: i64,
}

/// Asks the feed (retrying, except on the very first poll) and decides how much to lean on
/// GitHub this time:
///
/// - The very first poll (nothing has ever loaded, nothing saved): one try, then GitHub at once,
///   so the window fills.
/// - Until every app has loaded once: if the feed doesn't answer after its retries, GitHub is
///   asked fairly freely (every 5 minutes per app).
/// - After that: the feed's last answer stands in while it fails, and GitHub is asked only for
///   apps the feed doesn't list or hasn't refreshed in a day, or once the feed has seriously
///   failed (several checks in a row and an hour without an answer); then hourly per app.
fn feed_and_fallback(http: &Http, paths: &Paths, shared: &Shared) -> (Option<Feed>, Fallback) {
    // Open3D has no release-feed service: GitHub answers for every app,
    // the way it does when a feed is down, without counting that as a failure.
    let url = feed_url();
    if url.is_empty() {
        let loaded = shared.loaded.load(Ordering::SeqCst);
        return (None, Fallback { trust_feed: false, github_every: if loaded { GITHUB_FALLBACK_EVERY } else { EARLY_GITHUB_EVERY } });
    }
    let saved = load_feed(paths);
    let loaded = shared.loaded.load(Ordering::SeqCst);
    let first_time = !loaded && saved.is_none();
    let attempts = if first_time { 1 } else { FEED_ATTEMPTS };
    let fetched = fetch_feed(http, paths, &url, &shared.feed_throttle, attempts);
    let now = crate::now();
    let (fresh, answered, failures) = {
        let mut failures = shared.feed_failures.lock().unwrap_or_else(PoisonError::into_inner);
        match fetched {
            FeedFetch::Answered(feed) => {
                *failures = 0;
                *shared.feed_ok_at.lock().unwrap_or_else(PoisonError::into_inner) = Some(now);
                (Some(feed), true, 0)
            }
            FeedFetch::Failed => {
                *failures = failures.saturating_add(1);
                (None, false, *failures)
            }
            // Not asked this time: things stand as the last request left them.
            FeedFetch::Held => (None, *failures == 0 && saved.is_some(), *failures),
        }
    };
    let ok_at = *shared.feed_ok_at.lock().unwrap_or_else(PoisonError::into_inner);
    let fallback = decide_fallback(answered, loaded, first_time, failures, ok_at, now);
    (fresh.or(saved), fallback)
}

/// The policy in [`feed_and_fallback`], on its own so it can be tested.
fn decide_fallback(answered: bool, loaded: bool, first_time: bool, failures: u32, ok_at: Option<i64>, now: i64) -> Fallback {
    let github_every = if loaded { GITHUB_FALLBACK_EVERY } else { EARLY_GITHUB_EVERY };
    let seriously_failed =
        !answered && (first_time || !loaded || (failures >= SERIOUS_FEED_FAILURES && ok_at.is_none_or(|t| now - t >= SERIOUS_FEED_OUTAGE)));
    Fallback { trust_feed: !seriously_failed, github_every }
}

/// The latest release of `app`, which service it came from, and the GitHub route used (for the
/// rate-limit notice): the feed when it's trusted and vouches for the app, else GitHub if it's
/// due, else GitHub's last answer. No release when there's nothing new to say.
#[allow(clippy::too_many_arguments)]
fn latest_for(
    app: &SuiteApp,
    feed: Option<&Feed>,
    fallback: Fallback,
    source: &dyn ReleaseSource,
    shared: &Shared,
    pre: bool,
    force: bool,
    now: i64,
) -> (Option<LatestRelease>, Origin, Option<Source>) {
    let repo = feed_repo(feed, app);
    if fallback.trust_feed {
        match feed.map(|f| f.answer(app, now)) {
            // The feed can lag GitHub by some minutes after a release. A check the user pressed
            // looks past it when GitHub's quota-free redirect names something newer (#91).
            Some(FeedAnswer::Fresh(found)) if !(force && feed_is_behind(source, &repo, found.as_ref())) => {
                return (Some(found.map_or(LatestRelease::None, LatestRelease::Found)), Origin::ReleaseFeed, Some(Source::Feed));
            }
            Some(FeedAnswer::Fresh(_) | FeedAnswer::AskGitHub) | None => {}
        }
    }
    let due = {
        let mut asked = shared.github_asked.lock().unwrap_or_else(PoisonError::into_inner);
        let due = force || asked.get(app.id).is_none_or(|t| now - t >= fallback.github_every);
        if due {
            asked.insert(app.id.to_string(), now);
        }
        due
    };
    if due {
        let answer = source.latest(&repo, pre);
        (Some(answer.latest), Origin::GitHub, Some(answer.source))
    } else {
        (source.cached(&repo, pre), Origin::GitHub, None)
    }
}

/// Whether GitHub names a newer stable release of `repo` than the feed's `latest` (`None`: the
/// feed says there is none). `false` whenever the redirect can't be read: the feed then stands.
fn feed_is_behind(source: &dyn ReleaseSource, repo: &str, latest: Option<&Release>) -> bool {
    let Some(newest) = source.newest_tag(repo).and_then(|tag| version::parse(&tag)) else { return false };
    match latest.and_then(|r| r.version.as_ref()) {
        Some(feed) => version::is_newer(&newest, feed),
        None => true,
    }
}

/// Until when the footer says GitHub's limit holds details back: only while it does, that is
/// when this check had to settle for the release pages or saved answers. When the feed (or the
/// API itself) answered everything, nothing is missing, however recently GitHub said no.
fn limit_notice(source_used: Source, limited_until: Option<i64>) -> Option<i64> {
    limited_until.filter(|_| matches!(source_used, Source::GitHubPages | Source::Cache))
}

/// The repo to ask GitHub about, following the feed's repo renames.
fn feed_repo(feed: Option<&Feed>, app: &SuiteApp) -> String {
    feed.map_or_else(|| app.repo.to_string(), |f| f.repo_for(app))
}

/// What came of asking the feed during one check.
enum FeedFetch {
    Answered(Feed),
    /// Asked, without a good answer.
    Failed,
    /// Not asked: the [`FeedThrottle`] said to hold off.
    Held,
}

/// Asks Open3D's release feed up to `attempts` times, as often as `throttle` allows (waiting at
/// most [`MAX_THROTTLE_WAIT`] for it), saving a good answer.
fn fetch_feed(http: &Http, paths: &Paths, url: &str, throttle: &Mutex<FeedThrottle>, attempts: usize) -> FeedFetch {
    let mut asked = false;
    for attempt in 0..attempts.max(1) {
        let wait = throttle.lock().unwrap_or_else(PoisonError::into_inner).wait(crate::now());
        if wait > MAX_THROTTLE_WAIT {
            if !asked {
                log::info!("holding off the Open3D API for {wait} s");
            }
            break;
        }
        if wait > 0 {
            std::thread::sleep(Duration::from_secs(wait.unsigned_abs()));
        }
        {
            let mut throttle = throttle.lock().unwrap_or_else(PoisonError::into_inner);
            let now = crate::now();
            if throttle.wait(now) > 0 {
                break;
            }
            throttle.sending(now);
        }
        asked = true;
        let outcome = match http.get_feed(url) {
            Ok(body) => match Feed::parse(&body) {
                Some(feed) => {
                    if let Err(e) = crate::paths::write_atomic(&paths.feed_cache(), body.as_bytes()) {
                        log::warn!("couldn't save the release feed: {e}");
                    }
                    throttle.lock().unwrap_or_else(PoisonError::into_inner).finished(Outcome::Answered, crate::now(), 0.0);
                    return FeedFetch::Answered(feed);
                }
                None => {
                    log::warn!("the release feed answered with something unreadable (try {})", attempt + 1);
                    Outcome::Failed { retry_after: None }
                }
            },
            Err(e) if e.throttled => {
                log::warn!("the Open3D API asked us to slow down (HTTP 429); holding off");
                Outcome::Throttled { retry_after: e.retry_after }
            }
            Err(e) => {
                log::info!("release feed unavailable (try {}): {}", attempt + 1, e.why);
                Outcome::Failed { retry_after: e.retry_after }
            }
        };
        throttle.lock().unwrap_or_else(PoisonError::into_inner).finished(outcome, crate::now(), feed_throttle::jitter());
        if matches!(outcome, Outcome::Throttled { .. }) {
            break;
        }
    }
    if asked { FeedFetch::Failed } else { FeedFetch::Held }
}

/// The feed's address. `OPEN3D_LAUNCHER_FEED_URL` points at a staging server or a local copy
/// (only `https://`, or plain `http://` on localhost).
fn feed_url() -> String {
    std::env::var("OPEN3D_LAUNCHER_FEED_URL")
        .ok()
        .filter(|u| feed_url_override_allowed(u, cfg!(feature = "local-mirror")))
        .unwrap_or_else(|| open3d_launcher_core::feed::FEED_URL.to_string())
}

fn feed_url_override_allowed(url: &str, allow_https: bool) -> bool {
    is_loopback(url) || (allow_https && url.starts_with("https://"))
}

use crate::http::is_loopback;

/// `OPEN3D_LAUNCHER_DOWNLOAD_BASE`: a mirror on this machine (`cargo xtask local serve`) that
/// serves `release_info.json` and the builds it lists. While it's set, releases come only from
/// there (never GitHub or Open3D's API), and its download links are trusted. Loopback only, so
/// nothing but the developer's own builds can be installed this way, and only in builds with the
/// `local-mirror` feature: release builds ignore the variable.
fn local_mirror() -> Option<String> {
    if !cfg!(feature = "local-mirror") {
        return None;
    }
    let base = std::env::var("OPEN3D_LAUNCHER_DOWNLOAD_BASE").ok()?;
    let base = base.trim();
    if !is_loopback(base) || base.contains(['?', '#', '\\']) {
        log::warn!("ignoring OPEN3D_LAUNCHER_DOWNLOAD_BASE={base}: only http://127.0.0.1:<port>/, http://localhost:<port>/ or http://[::1]:<port>/");
        return None;
    }
    Some(if base.ends_with('/') { base.to_string() } else { format!("{base}/") })
}

/// The local mirror's feed: `OPEN3D_LAUNCHER_FEED_URL` if set, else `release_info.json` beside
/// the builds. Asked every check (it's on this machine), and never saved over Open3D's.
fn local_feed(http: &Http, base: &str) -> Option<Feed> {
    let url = std::env::var("OPEN3D_LAUNCHER_FEED_URL").ok().filter(|u| is_loopback(u)).unwrap_or_else(|| format!("{base}release_info.json"));
    match http.get_feed(&url) {
        Ok(body) => {
            let feed = Feed::parse(&body).map(|f| f.with_mirror(base));
            if feed.is_none() {
                log::warn!("the local mirror's feed at {url} is unreadable");
            }
            feed
        }
        Err(e) => {
            log::warn!("local mirror unavailable: {} (is `cargo xtask local serve` running?)", e.why);
            None
        }
    }
}

/// `app`'s latest release according to the local mirror. An app it doesn't list has no release.
fn mirror_latest(app: &SuiteApp, feed: Option<&Feed>, now: i64) -> (Option<LatestRelease>, Origin, Option<Source>) {
    let latest = match feed.map(|f| f.answer(app, now)) {
        Some(FeedAnswer::Fresh(Some(r))) => LatestRelease::Found(r),
        Some(FeedAnswer::Fresh(None) | FeedAnswer::AskGitHub) => LatestRelease::None,
        None => LatestRelease::Failed("the local mirror didn't answer".into()),
    };
    (Some(latest), Origin::ReleaseFeed, Some(Source::Feed))
}

/// A selected track never silently falls back to the mirror's stable release.
fn mirror_chosen(app: &SuiteApp, feed: Option<&Feed>, now: i64, track: &VersionTrack) -> (Option<LatestRelease>, Origin, Option<Source>) {
    let (answer, origin, source) = mirror_latest(app, feed, now);
    let chosen = match (track, answer) {
        (VersionTrack::Stable, answer) => answer,
        (VersionTrack::Nightly, Some(LatestRelease::Found(r))) if open3d_launcher_core::release::is_nightly(&r) => Some(LatestRelease::Found(r)),
        (VersionTrack::Pinned(tag), Some(LatestRelease::Found(r))) if &r.tag == tag => Some(LatestRelease::Found(r)),
        (_, Some(LatestRelease::Failed(why))) => Some(LatestRelease::Failed(why)),
        _ => Some(LatestRelease::None),
    };
    (chosen, origin, source)
}

/// The last saved feed answer, if any.
fn load_feed(paths: &Paths) -> Option<Feed> {
    std::fs::read_to_string(paths.feed_cache()).ok().and_then(|body| Feed::parse(&body))
}

/// The machine's real architecture: an x64 launcher on Windows on ARM should still pick ARM64
/// builds.
fn native_platform() -> Option<Platform> {
    let mut p = Platform::current()?;
    if p.os == Os::Windows {
        let arch = std::env::var("PROCESSOR_ARCHITEW6432").or_else(|_| std::env::var("PROCESSOR_ARCHITECTURE")).unwrap_or_default();
        if arch.eq_ignore_ascii_case("ARM64") {
            p.arch = Arch::Arm64;
        } else if arch.eq_ignore_ascii_case("AMD64") {
            p.arch = Arch::X64;
        }
    }
    Some(p)
}

/// Clears an app's "pictures on their way" mark when the thread fetching them ends, panic or not.
struct PicturesBusy {
    shared: Arc<Shared>,
    app: &'static str,
}

impl Drop for PicturesBusy {
    fn drop(&mut self) {
        self.shared.pictures_busy.lock().unwrap_or_else(PoisonError::into_inner).remove(self.app);
    }
}

impl NativeBackend {
    /// Starts fetching the pictures of an app's details page, unless they are already on their way.
    /// An app without a page, or a name that isn't an app, is ignored.
    fn fetch_screenshots(&self, app: &str) {
        let Some(details) = open3d_launcher_core::details::AppDetails::by_id(app) else { return };
        if !self.shared.pictures_busy.lock().unwrap_or_else(PoisonError::into_inner).insert(details.id) {
            return;
        }
        let (shared, http, paths) = (self.shared.clone(), self.http.clone(), self.paths.clone());
        std::thread::spawn(move || {
            let _busy = PicturesBusy { shared: shared.clone(), app: details.id };
            crate::screenshots::fetch_all(http.as_ref(), &paths, details, |key, result| {
                shared.emit(Event::Screenshot { app: details.id.to_string(), key: key.to_string(), result });
            });
        });
    }
}

impl Backend for NativeBackend {
    fn send(&mut self, command: Command) {
        match command {
            Command::CheckForUpdates { force } => self.check(force),
            Command::Detect => self.detect_all(),
            Command::ListVersions { app } => {
                let Some(def) = (if app == LAUNCHER_ID { Some(&LAUNCHER) } else { SuiteApp::by_id(&app) }) else { return };
                let source = self.source.clone();
                let shared = self.shared.clone();
                let repo = feed_repo(load_feed(&self.paths).as_ref(), def);
                std::thread::spawn(move || {
                    let result = source.ok_or("secure connections aren't available on this system".to_string()).and_then(|s| s.versions(&repo));
                    shared.emit(Event::Versions { app, result });
                });
            }
            Command::Install { app } => {
                self.start_job(&app, Work::Install);
            }
            Command::InstallAll { apps } => self.install_all(&apps),
            Command::Replace { app, path } => {
                self.start_job(&app, Work::Replace(path));
            }
            Command::Uninstall { app } => {
                self.start_job(&app, Work::Uninstall);
            }
            Command::Pause { app } => {
                if let Some(stop) = self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner).get(&app) {
                    stop.pause();
                }
            }
            Command::Cancel { app } => {
                let running = self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner).get(&app).cloned();
                match running {
                    Some(stop) => stop.cancel(),
                    // A paused download, or one kept after a failed install: throw the file away.
                    None => {
                        if let Some(def) = SuiteApp::by_id(&app)
                            && let Some((dest, _, size)) = planned_download(def, &self.shared, &self.paths, self.platform, &self.options())
                        {
                            http::discard(&dest, size);
                        }
                        self.shared.emit(Event::Paused { app: app.clone(), download: None });
                        self.shared.emit(Event::ReadyToInstall { app, ready: false });
                    }
                }
            }
            Command::Launch { app, args } => {
                let Some(def) = SuiteApp::by_id(&app) else { return };
                let result = match self.installed(&app) {
                    Some(inst) => install::launch(def, &inst, &self.paths, &args),
                    None => Err(format!("{} isn't installed", def.name)),
                };
                if let Err(e) = result {
                    self.shared.emit(Event::JobFinished { app, result: Err(e) });
                }
            }
            Command::Reveal { app } => {
                if let Some(inst) = self.installed(&app) {
                    let path = std::path::PathBuf::from(&inst.location);
                    let result = if path.exists() { crate::process::reveal(&path) } else { Err("it isn't a file on this computer".into()) };
                    if let Err(e) = result {
                        self.shared.emit(Event::JobFinished { app, result: Err(format!("Couldn't show it: {e}")) });
                    }
                }
            }
            Command::UpdateLauncher => self.update_launcher(),
            Command::FetchScreenshots { app } => self.fetch_screenshots(&app),
            Command::Configure(options) => {
                let changed_pre = {
                    let mut o = self.shared.options.lock().unwrap_or_else(PoisonError::into_inner);
                    let changed =
                        o.prereleases != options.prereleases || o.versions != options.versions || o.launcher_version != options.launcher_version;
                    *o = options;
                    // An older worker must not overwrite the plan for these settings.
                    for cached in self.shared.download_sizes.lock().unwrap_or_else(PoisonError::into_inner).values_mut() {
                        cached.invalidate();
                    }
                    changed
                };
                // Every Configure, not only on a change: the first one carries the choices saved
                // before the shared file existed, and the file may have been deleted since.
                if let Some(path) = &self.pins {
                    match crate::pins::sync(path, &self.options().versions) {
                        Ok(true) => log::info!("version choices written to {}", path.display()),
                        Ok(false) => {}
                        Err(e) => log::warn!("couldn't write the version choices: {e}"),
                    }
                }
                // Another package format means another file to download.
                self.replan();
                if changed_pre {
                    self.check(true);
                }
            }
        }
    }

    fn poll(&mut self) -> Vec<Event> {
        std::mem::take(&mut *self.shared.events.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn launcher_kind(&self) -> Option<PackageKind> {
        // Looked up (it can run package-manager queries) with a newer launcher release; never here.
        self.shared.self_install.get().and_then(Option::as_ref).map(crate::self_update::kind)
    }

    fn capabilities(&self) -> Capabilities {
        let reveal_label = match self.platform.map(|p| p.os) {
            Some(Os::Windows) => "Show in Explorer",
            _ => "Show in Files",
        };
        Capabilities { manage_apps: true, reveal_label, self_update: self.shared.self_install.get().is_some_and(Option::is_some) }
    }

    fn platform(&self) -> Option<Platform> {
        self.platform
    }

    fn download_size(&self, app: &str) -> Option<(PackageKind, u64)> {
        self.shared.download_sizes.lock().unwrap_or_else(PoisonError::into_inner).get(app).and_then(|cached| cached.plan)
    }

    fn now(&self) -> i64 {
        crate::now()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    use open3d_launcher_core::protocol::LatestRelease;

    use super::*;

    use crate::source::Answer;

    /// A "Replace" answer covers the path that was shown, never a destination that changed since.
    #[test]
    fn replacing_needs_consent_for_the_exact_path_in_the_way() {
        let shown = std::path::Path::new("/home/a/Applications/phototool-0.5.0-linux-x86_64.AppImage");
        let moved = std::path::Path::new("/home/a/Applications/phototool-0.6.0-linux-x86_64.AppImage");
        let approved = shown.display().to_string();
        assert_eq!(replace_consent(None, None), Consent::Free);
        assert_eq!(replace_consent(None, Some(&approved)), Consent::Free, "nothing in the way: nothing to overwrite");
        assert_eq!(replace_consent(Some(shown), None), Consent::Ask(shown));
        assert_eq!(replace_consent(Some(shown), Some(&approved)), Consent::Given);
        assert_eq!(replace_consent(Some(moved), Some(&approved)), Consent::Ask(moved), "a yes for one file must not overwrite another");
    }

    #[test]
    fn an_install_cannot_use_a_stale_release_from_a_different_track() {
        let mut release = Release { tag: "v0.8.0".into(), version: version::parse("v0.8.0"), ..Default::default() };
        assert!(!release_matches_track(&release, &VersionTrack::Nightly, false));
        assert!(!release_matches_track(&release, &VersionTrack::Pinned("v0.7.0".into()), false));
        assert!(release_matches_track(&release, &VersionTrack::Stable, false));
        release.tag = "v0.9.0-nightly.20261028".into();
        release.version = version::parse(&release.tag);
        release.prerelease = true;
        assert!(release_matches_track(&release, &VersionTrack::Nightly, false));
        assert!(!release_matches_track(&release, &VersionTrack::Stable, false));
    }

    #[test]
    fn feed_url_overrides_are_loopback_only_outside_local_mirror_builds() {
        assert!(feed_url_override_allowed("http://127.0.0.1:8787/release_info.json", false));
        assert!(feed_url_override_allowed("http://localhost:8787/release_info.json", false));
        assert!(!feed_url_override_allowed("https://staging.example/release_info.json", false));
        assert!(feed_url_override_allowed("https://staging.example/release_info.json", true));
        assert!(!feed_url_override_allowed("http://attacker.example/release_info.json", true));
    }

    /// GitHub, counting how often it's asked.
    #[derive(Default)]
    struct CountingGitHub(AtomicUsize);

    impl ReleaseSource for CountingGitHub {
        fn latest(&self, _repo: &str, _pre: bool) -> Answer {
            self.0.fetch_add(1, Ordering::SeqCst);
            Answer { latest: LatestRelease::None, source: Source::GitHubApi }
        }
        fn cached(&self, _repo: &str, _pre: bool) -> Option<LatestRelease> {
            None
        }
        fn rate_limited_until(&self) -> Option<i64> {
            None
        }
        fn save(&self) {}
    }

    fn shared() -> Shared {
        Shared {
            events: Mutex::new(Vec::new()),
            reporting: Mutex::new(()),
            system: Mutex::new(None),
            planning: Mutex::new(()),
            waker: Arc::new(|| {}),
            latest: Mutex::new(HashMap::new()),
            download_sizes: Mutex::new(HashMap::new()),
            installed: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            restarting: AtomicBool::new(false),
            manifest: Mutex::new(Manifest::default()),
            options: Mutex::new(BackendOptions::default()),
            checking: AtomicBool::new(false),
            detecting: AtomicBool::new(false),
            last_check: Mutex::new(None),
            launcher_release: Mutex::new(None),
            self_install: OnceLock::new(),
            github_asked: Mutex::new(HashMap::new()),
            answered: Mutex::new(std::collections::HashSet::new()),
            loaded: AtomicBool::new(false),
            feed_failures: Mutex::new(0),
            feed_ok_at: Mutex::new(None),
            feed_throttle: Mutex::new(FeedThrottle::new()),
            pictures_busy: Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// A local server answering every request with `status` (and `Retry-After: 120` on a 429),
    /// counting the requests.
    fn serve(status: &'static str) -> (String, Arc<AtomicUsize>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/craft_apps/release_info", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                counted.fetch_add(1, Ordering::SeqCst);
                let extra = if status.starts_with("429") { "Retry-After: 120\r\n" } else { "" };
                let _ = write!(stream, "HTTP/1.1 {status}\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n");
            }
        });
        (url, hits)
    }

    #[test]
    fn a_429_stops_the_retries_and_holds_off_the_api() {
        let (url, hits) = serve("429 Too Many Requests");
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let http = Http::new().unwrap();
        let throttle = Mutex::new(FeedThrottle::load(&paths.feed_throttle(), crate::now()));
        assert!(matches!(fetch_feed(&http, &paths, &url, &throttle, FEED_ATTEMPTS), FeedFetch::Failed));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // The next checks (and a relaunch) don't call it at all.
        assert!(matches!(fetch_feed(&http, &paths, &url, &throttle, FEED_ATTEMPTS), FeedFetch::Held));
        let relaunched = Mutex::new(FeedThrottle::load(&paths.feed_throttle(), crate::now()));
        assert!(matches!(fetch_feed(&http, &paths, &url, &relaunched, FEED_ATTEMPTS), FeedFetch::Held));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failing_api_is_retried_quickly_only_during_the_first_load() {
        let (url, hits) = serve("503 Service Unavailable");
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let http = Http::new().unwrap();
        let throttle = Mutex::new(FeedThrottle::new());
        // First load: three tries, a couple of seconds apart.
        assert!(matches!(fetch_feed(&http, &paths, &url, &throttle, FEED_ATTEMPTS), FeedFetch::Failed));
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        // Once the server has answered, never more than one a minute.
        throttle.lock().unwrap().finished(Outcome::Answered, crate::now(), 0.0);
        assert!(matches!(fetch_feed(&http, &paths, &url, &throttle, FEED_ATTEMPTS), FeedFetch::Held));
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    const NOW: i64 = 1_791_500_000;

    #[test]
    fn the_limit_notice_shows_only_while_details_are_missing() {
        let until = Some(NOW + 31 * 60);
        // GitHub said no earlier, but the feed has since answered every app: nothing to warn about.
        assert_eq!(limit_notice(Source::Feed, until), None);
        assert_eq!(limit_notice(Source::GitHubApi, until), None);
        // Some apps came from the release pages or saved answers: say when the rest returns.
        assert_eq!(limit_notice(Source::GitHubPages, until), until);
        assert_eq!(limit_notice(Source::Cache, until), until);
        assert_eq!(limit_notice(Source::Cache, None), None);
    }

    #[test]
    fn the_first_poll_tries_once_and_falls_back_at_once() {
        // Nothing has ever loaded and the feed didn't answer: GitHub, freely.
        let f = decide_fallback(false, false, true, 1, None, NOW);
        assert_eq!(f, Fallback { trust_feed: false, github_every: EARLY_GITHUB_EVERY });
        // The feed answered: trust it.
        assert!(decide_fallback(true, false, true, 0, Some(NOW), NOW).trust_feed);
    }

    #[test]
    fn before_everything_loads_github_is_asked_more_eagerly() {
        // A saved feed but not everything loaded yet, and the feed isn't answering.
        let f = decide_fallback(false, false, false, 1, None, NOW);
        assert_eq!(f, Fallback { trust_feed: false, github_every: EARLY_GITHUB_EVERY });
    }

    #[test]
    fn after_loading_github_waits_for_a_serious_feed_failure() {
        // A blip or two: keep showing the feed's last answer.
        for failures in 1..SERIOUS_FEED_FAILURES {
            let f = decide_fallback(false, true, false, failures, Some(NOW - 10 * 60), NOW);
            assert_eq!(f, Fallback { trust_feed: true, github_every: GITHUB_FALLBACK_EVERY }, "{failures}");
        }
        // Several checks in a row, but it answered recently: still trusted.
        assert!(decide_fallback(false, true, false, SERIOUS_FEED_FAILURES, Some(NOW - 20 * 60), NOW).trust_feed);
        // Several checks in a row and an hour without an answer: GitHub takes over, hourly.
        let f = decide_fallback(false, true, false, SERIOUS_FEED_FAILURES, Some(NOW - SERIOUS_FEED_OUTAGE), NOW);
        assert_eq!(f, Fallback { trust_feed: false, github_every: GITHUB_FALLBACK_EVERY });
    }

    const APP: &str = "phototool";

    /// A PhotoTool release with one file per platform and format, so whichever this machine
    /// installs is there.
    fn release(version: &str) -> Release {
        let suffixes = ["-portable.zip", ".tar.gz", ".msi", ".dmg", ".AppImage", ".deb", ".rpm", ".flatpak"];
        let tokens = [
            "windows-x64",
            "windows-x86",
            "windows-arm64",
            "macos-universal",
            "linux-x86_64",
            "linux-aarch64",
            "linux-riscv64",
            "freebsd-x86_64",
            "freebsd-aarch64",
        ];
        let assets = tokens
            .iter()
            .flat_map(|t| suffixes.iter().map(move |s| format!("{APP}-{version}-{t}{s}")))
            .map(|name| Asset { url: format!("https://example.invalid/{name}"), name, size: 3, sha256: None })
            .collect();
        Release { assets, ..Release::default() }
    }

    /// A backend that knows `release("0.5.0")` and keeps its files under `root`.
    fn backend(root: &std::path::Path, platform: Platform, source: Option<Arc<dyn ReleaseSource>>) -> NativeBackend {
        let shared = shared();
        shared.latest.lock().unwrap().insert(APP.to_string(), release("0.5.0"));
        let shared = Arc::new(shared);
        NativeBackend { shared, paths: Paths::under(root), http: None, source, platform: Some(platform), pins: Some(root.join("versions.json")) }
    }

    fn wait_until(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready() {
            assert!(Instant::now() < deadline, "the worker did not finish unwinding");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn panic_on_wake() -> (Waker, mpsc::Receiver<()>) {
        let (sent, received) = mpsc::channel();
        (
            Arc::new(move || {
                let _ = sent.send(());
                panic!("injected worker panic")
            }),
            received,
        )
    }

    #[test]
    fn a_panicking_detection_worker_releases_its_busy_flag() {
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        let (waker, woke) = panic_on_wake();
        Arc::get_mut(&mut backend.shared).unwrap().waker = waker;

        backend.detect_all();
        woke.recv_timeout(Duration::from_secs(2)).unwrap();
        wait_until(|| !backend.shared.detecting.load(Ordering::SeqCst));
    }

    #[test]
    fn a_panicking_check_worker_releases_its_busy_flag() {
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, Some(Arc::new(CountingGitHub::default())));
        backend.http = Some(Http::new().unwrap());
        let (waker, woke) = panic_on_wake();
        Arc::get_mut(&mut backend.shared).unwrap().waker = waker;

        backend.check(true);
        woke.recv_timeout(Duration::from_secs(2)).unwrap();
        wait_until(|| !backend.shared.checking.load(Ordering::SeqCst));
    }

    #[test]
    fn the_settings_reach_the_file_the_apps_read() {
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        let mut options = BackendOptions::default();
        options.versions.insert("pdftool".into(), VersionTrack::Pinned("v0.13.0".into()));
        backend.send(Command::Configure(options));
        let pins = craft_update::pins::Pins::load(&dir.path().join("versions.json"));
        assert_eq!(pins.track("pdftool"), Some(craft_update::pins::VersionTrack::Pinned("v0.13.0".into())));
        backend.send(Command::Configure(BackendOptions::default()));
        let pins = craft_update::pins::Pins::load(&dir.path().join("versions.json"));
        assert_eq!(pins.choice("pdftool"), craft_update::pins::Choice::Stable);
    }

    #[test]
    fn the_launcher_restarts_only_once_every_other_job_has_ended() {
        let jobs = Mutex::new(HashMap::new());
        for id in [LAUNCHER_ID, "phototool", "filmtool"] {
            jobs.lock().unwrap().insert(id.to_string(), Arc::new(Stop::default()));
        }
        let restarting = AtomicBool::new(false);
        let stop = Stop::default();
        let (waiting, waits) = mpsc::channel();
        std::thread::scope(|scope| {
            let (jobs, restarting, stop) = (&jobs, &restarting, &stop);
            let claim = scope.spawn(move || claim_restart(jobs, restarting, stop, RESTART_WAIT, &mut || waiting.send(()).unwrap()));
            waits.recv().unwrap();
            // One install ends; another is still running or queued.
            jobs.lock().unwrap().remove("phototool");
            std::thread::sleep(Duration::from_millis(250));
            assert!(!claim.is_finished() && !restarting.load(Ordering::SeqCst));
            jobs.lock().unwrap().remove("filmtool");
            assert_eq!(claim.join().unwrap(), Ok(()));
        });
        assert!(restarting.load(Ordering::SeqCst));
        assert!(waits.try_recv().is_err(), "it says it is waiting once");

        // With nothing else running there is nothing to wait for.
        let restarting = AtomicBool::new(false);
        assert_eq!(claim_restart(&jobs, &restarting, &stop, RESTART_WAIT, &mut || panic!("it never waits")), Ok(()));
        assert!(restarting.load(Ordering::SeqCst));
    }
}
