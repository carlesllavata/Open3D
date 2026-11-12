// SPDX-License-Identifier: GPL-3.0-or-later
//! Drives the launcher UI against a fake backend: what's shown for each status, and which
//! commands the buttons send. `OPEN3D_LAUNCHER_SNAPSHOTS=<dir>` also saves screenshots; run that
//! with `-- --test-threads=1`, as rendering on several threads at once can crash Vulkan loaders
//! older than 1.4.350 (KhronosGroup/Vulkan-Loader#1863).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::OnceLock;

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use open3d_launcher_core::details::AppDetails;
use open3d_launcher_core::protocol::{Backend, Capabilities, Command, Event, JobPhase, LatestRelease, Origin, PausedDownload, Source};
use open3d_launcher_core::release::{Asset, Release};
use open3d_launcher_core::status::InstallSource;
use open3d_launcher_core::{Arch, Installed, Os, Platform, SUITE, version};
use open3d_launcher_ui_egui::settings::ViewMode;
use open3d_launcher_ui_egui::{Host, LauncherUi, Settings};

const NOW: i64 = 1_791_500_000;

/// How the fake backend answers a request for an app's details pictures, whenever it is polled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Pictures {
    /// Every picture arrives.
    #[default]
    Online,
    /// Nothing is answered yet: the requests wait until the mode changes.
    Silent,
}

/// A small JPEG, made here so that no picture is kept in the repository for the tests.
fn test_jpeg() -> Vec<u8> {
    static JPEG: OnceLock<Vec<u8>> = OnceLock::new();
    JPEG.get_or_init(|| {
        let (w, h) = (320u32, 200u32);
        let rgb: Vec<u8> = (0..h).flat_map(|y| (0..w).flat_map(move |x| [(x * 255 / w) as u8, (y * 255 / h) as u8, 160])).collect();
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80).encode(&rgb, w, h, image::ExtendedColorType::Rgb8).unwrap();
        out
    })
    .clone()
}

struct Fake {
    events: Vec<Event>,
    sent: Rc<RefCell<Vec<Command>>>,
    manage: bool,
    pictures: Rc<Cell<Pictures>>,
    /// Apps whose pictures were asked for and not answered yet.
    asked: Vec<String>,
    downloads: std::collections::BTreeMap<String, (open3d_launcher_core::PackageKind, u64)>,
    clock: Rc<Cell<i64>>,
}

impl Backend for Fake {
    fn send(&mut self, command: Command) {
        if let Command::FetchScreenshots { app } = &command {
            self.asked.push(app.clone());
        }
        self.sent.borrow_mut().push(command);
    }
    fn poll(&mut self) -> Vec<Event> {
        for event in &self.events {
            if let Event::Latest { app, latest: LatestRelease::Found(release), .. } = event
                && let Some(def) = open3d_launcher_core::SuiteApp::by_id(app)
            {
                let platform = self.platform().unwrap();
                let stems: Vec<_> = def.stems().collect();
                if let Some((asset, kind)) = open3d_launcher_core::platform::pick_asset(release, &stems, platform, def.kinds()) {
                    self.downloads.insert(app.clone(), (kind, asset.size));
                }
            }
        }
        let mut out = std::mem::take(&mut self.events);
        if self.pictures.get() != Pictures::Silent {
            for app in std::mem::take(&mut self.asked) {
                let Some(page) = AppDetails::by_id(&app) else { continue };
                for shot in page.shots {
                    out.push(Event::Screenshot { app: app.clone(), key: shot.key.to_string(), result: Ok(test_jpeg()) });
                }
            }
        }
        out
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { manage_apps: self.manage, reveal_label: "Show in Explorer", self_update: self.manage }
    }
    fn platform(&self) -> Option<Platform> {
        Some(Platform { os: Os::Windows, arch: Arch::X64 })
    }
    fn now(&self) -> i64 {
        self.clock.get()
    }
    fn download_size(&self, app: &str) -> Option<(open3d_launcher_core::PackageKind, u64)> {
        self.downloads.get(app).copied()
    }
}

struct PlatformFake {
    inner: Fake,
    platform: Platform,
}
impl Backend for PlatformFake {
    fn send(&mut self, command: Command) {
        self.inner.send(command);
    }
    fn poll(&mut self) -> Vec<Event> {
        self.inner.poll()
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    fn platform(&self) -> Option<Platform> {
        Some(self.platform)
    }
    fn now(&self) -> i64 {
        self.inner.now()
    }
}

struct NoHost;
impl Host for NoHost {}

/// A harness that has the launcher's fonts, theme and image loaders before its first pass, and
/// never paints a synthetic cursor into saved images.
fn test_harness_builder<State>(theme: egui::Theme) -> egui_kittest::HarnessBuilder<State> {
    let choice = if theme == egui::Theme::Dark {
        open3d_launcher_ui_egui::theme::ThemeChoice::Dark
    } else {
        open3d_launcher_ui_egui::theme::ThemeChoice::Light
    };
    egui_kittest::HarnessBuilder::default()
        // 0.37 defaults: strict accessible names and a panic on a missing glyph. Neither is
        // relaxed here; fixed controls are painter-drawn and dynamic notes/feed text uses the
        // attributed monochrome emoji fonts.
        .with_accessibility_check(true)
        .with_render_cursor(false)
        .with_context_setup(move |ctx| {
            open3d_launcher_ui_egui::theme::install_fonts(ctx);
            egui_extras::install_image_loaders(ctx);
            open3d_launcher_ui_egui::theme::apply(ctx, choice);
        })
}

fn release(id: &str, v: &str, at: i64) -> Release {
    // The launcher ships an MSI; the apps ship 3ds Max plug-ins.
    let name = if id == "open3d-launcher" { format!("{id}-{v}-windows-x64.msi") } else { format!("{id}-{v}-windows-x64-3dsmax.zip") };
    Release {
        tag: format!("v{v}"),
        version: version::parse(v),
        name: format!("v{v}"),
        published_at: Some(at),
        html_url: format!("https://github.com/carlesllavata/{id}/releases/tag/v{v}"),
        notes: "## What's Changed\n* Faster things by @a in https://github.com/carlesllavata/x/pull/1".into(),
        prerelease: false,
        assets: vec![Asset {
            name: name.clone(),
            size: 150 << 20,
            url: format!("https://github.com/carlesllavata/{id}/releases/download/v{v}/{name}"),
            sha256: None,
        }],
    }
}

/// A realistic mix: updates, up-to-date apps, a download in progress, one app with no release.
fn events() -> Vec<Event> {
    let mut ev = Vec::new();
    let versions = [
        ("phototool", "0.5.0"),
        ("vectortool", "0.7.0"),
        ("filmtool", "0.4.0"),
        ("lighttool", "0.4.0"),
        ("pdftool", "0.4.0"),
        ("effecttool", "0.6.0"),
        ("designtool", "0.4.0"),
        ("wordtool", "0.3.0"),
        ("decktool", "0.3.0"),
        ("gridtool", "0.3.0"),
        ("cadtool", "0.3.0"),
    ];
    for (i, (id, v)) in versions.iter().enumerate() {
        ev.push(Event::Latest {
            app: (*id).into(),
            latest: LatestRelease::Found(release(id, v, NOW - 86_400 - i as i64 * 600)),
            from: Origin::ReleaseFeed,
        });
    }
    ev.push(Event::Latest { app: "soundtool".into(), latest: LatestRelease::None, from: Origin::ReleaseFeed });
    let installed = [("phototool", "0.4.1"), ("lighttool", "0.4.0"), ("wordtool", "0.3.0"), ("effecttool", "0.5.0")];
    for app in &SUITE {
        let inst = installed.iter().find(|(id, _)| *id == app.id).map(|(_, v)| Installed {
            version: version::parse(v),
            source: InstallSource::Bundle,
            location: format!(r"C:\Users\me\AppData\Roaming\Autodesk\ApplicationPlugins\{}.bundle", app.name),
            package: None,
            externally_managed: false,
            origin: None,
            installation: None,
        });
        ev.push(Event::Detected { app: app.id.into(), installed: inst, other_copies: Vec::new() });
    }
    ev.push(Event::Progress { app: "filmtool".into(), phase: JobPhase::Downloading { done: 48 << 20, total: 131 << 20 } });
    ev.push(Event::Paused { app: "decktool".into(), download: Some(PausedDownload { done: 30 << 20, total: 120 << 20 }) });
    ev.push(Event::Progress { app: "gridtool".into(), phase: JobPhase::Reconnecting { done: 10 << 20, total: 90 << 20 } });
    ev.push(Event::ReadyToInstall { app: "cadtool".into(), ready: true });
    ev.push(Event::CheckFinished { at: NOW - 180, source: Source::GitHubApi, rate_limited_until: None });
    ev
}

fn harness(view: ViewMode, theme: egui::Theme, manage: bool) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    harness_at(view, theme, manage, 1.0)
}

fn harness_at(view: ViewMode, theme: egui::Theme, manage: bool, scale: f32) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    harness_with(events(), view, theme, manage, scale)
}

fn harness_with(
    events: Vec<Event>,
    view: ViewMode,
    theme: egui::Theme,
    manage: bool,
    scale: f32,
) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    harness_full(events, view, theme, manage, scale, open3d_launcher_ui_egui::TitleBar::Native, Rc::default(), None, None)
}

fn harness_with_title_bar(
    events: Vec<Event>,
    view: ViewMode,
    theme: egui::Theme,
    manage: bool,
    scale: f32,
    bar: open3d_launcher_ui_egui::TitleBar,
) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    harness_full(events, view, theme, manage, scale, bar, Rc::default(), None, None)
}

#[allow(clippy::too_many_arguments)] // Test fixture inputs vary independently across existing cases.
/// Builds a harness with the full fixture matrix (events, view, theme, manage mode, scale, title
/// bar, pictures, platform and community URL), forces English labels, and returns the recorded
/// commands.
fn harness_full(
    events: Vec<Event>,
    view: ViewMode,
    theme: egui::Theme,
    manage: bool,
    scale: f32,
    bar: open3d_launcher_ui_egui::TitleBar,
    pictures: Rc<Cell<Pictures>>,
    platform: Option<Platform>,
    community_url: Option<&'static str>,
) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    struct HeaderHost(open3d_launcher_ui_egui::TitleBar, Option<&'static str>);
    impl Host for HeaderHost {
        fn title_bar(&self) -> open3d_launcher_ui_egui::TitleBar {
            self.0
        }
        fn community_url(&self) -> &'static str {
            self.1.unwrap_or(open3d_launcher_core::catalog::COMMUNITY_URL)
        }
    }
    // Integration tests compile without cfg!(test), so system_lang() picks up the
    // developer's locale. Force English so labels match the assertions.
    open3d_launcher_ui_egui::i18n::set_test_lang(open3d_launcher_ui_egui::i18n::Lang::EN);
    let sent = Rc::new(RefCell::new(Vec::new()));
    let sent2 = sent.clone();
    let fake = Fake { events, sent: sent2, manage, pictures, asked: Vec::new(), downloads: Default::default(), clock: Rc::new(Cell::new(NOW)) };
    let mut backend: Option<Box<dyn Backend>> = Some(match platform {
        Some(platform) => Box::new(PlatformFake { inner: fake, platform }),
        None => Box::new(fake),
    });
    let settings = Settings { view, notify: false, ..Settings::default() };
    // No `.wgpu()`: it gives every test its own GPU device, and those segfault on parallel test threads (see above).
    // The default renderer creates one only when a test renders.
    let harness = test_harness_builder(theme).with_size(egui::vec2(1180.0, 1200.0)).with_pixels_per_point(scale).with_theme(theme).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                let mut s = settings.clone();
                s.theme = if theme == egui::Theme::Dark {
                    open3d_launcher_ui_egui::theme::ThemeChoice::Dark
                } else {
                    open3d_launcher_ui_egui::theme::ThemeChoice::Light
                };
                *state = Some(LauncherUi::new(ui.ctx(), b, s));
            }
            if let Some(l) = state.as_mut() {
                l.logic(&ui.ctx().clone(), &mut HeaderHost(bar, community_url));
                l.ui(ui, &mut HeaderHost(bar, community_url));
            }
        },
        None,
    );
    (harness, sent)
}

/// Save a PNG under `OPEN3D_LAUNCHER_SNAPSHOTS` when that is set. `name` is a test-owned file
/// stem (the test names and sizes make each one unique), never a path: a separator is rejected so
/// no caller can write outside the evidence directory.
fn snapshot(h: &mut Harness<'_, Option<LauncherUi>>, name: &str) {
    let Some(dir) = std::env::var_os("OPEN3D_LAUNCHER_SNAPSHOTS") else { return };
    assert!(
        !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != "..",
        "snapshot name must be one pathless file stem, got {name:?}"
    );
    let img = h.render().unwrap();
    img.save(std::path::Path::new(&dir).join(format!("{name}.png"))).unwrap();
}

#[test]
fn version_choices_have_room_and_nightly_scrolls_with_the_versions() {
    let releases: Vec<_> = (1..=25)
        .rev()
        .map(|v| {
            let mut r = release("open3d-launcher", &format!("0.{v}.0"), NOW);
            r.assets[0].sha256 = Some("0".repeat(64));
            r
        })
        .collect();
    let mut ev = events();
    ev.push(Event::Versions { app: open3d_launcher_core::catalog::LAUNCHER_ID.into(), result: Ok(releases) });
    let (mut h, _) = harness_with(ev, ViewMode::Grid, egui::Theme::Dark, true, 1.0);
    h.set_size(egui::vec2(1180.0, 1100.0));
    h.run_steps(6);
    h.get_by_label("Settings").click();
    h.run_steps(4);
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Launcher version").click();
    h.run_steps(5);
    let nightly_before = h.get_by_label("Nightly (none published yet)").rect();
    // The newest release carries the label; there is no separate "Latest release" row.
    let first = h.get_by_label("v0.25.0 (latest release)").rect();
    assert!(h.query_by_label("Latest release").is_none(), "the latest version is labelled in place");
    assert!(nightly_before.height() >= 40.0, "dropdown rows need a comfortable target: {nightly_before:?}");
    // Rows touch, so a selected row's highlight fills its whole slot and the spacing stays even.
    assert!((first.top() - nightly_before.bottom()).abs() < 1.0, "rows sit at an even pitch: {nightly_before:?} {first:?}");
    h.event(egui::Event::PointerMoved(first.center()));
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -250.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: egui::Modifiers::NONE,
    });
    h.run_steps(30);
    let nightly_after = h.get_by_label("Nightly (none published yet)").rect();
    assert!(nightly_after.top() < nightly_before.top() - 80.0, "Nightly must scroll with release rows: {nightly_before:?} -> {nightly_after:?}");
}

/// The date and size painted beside the version row named `label`, and their colour. `None` when
/// the row paints nothing beside its name.
fn painted_facts(h: &Harness<'static, Option<LauncherUi>>, label: &str) -> Option<(String, egui::Color32)> {
    fn texts(shape: &egui::Shape, out: &mut Vec<(egui::Pos2, String, egui::Color32)>) {
        match shape {
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| texts(s, out)),
            egui::Shape::Text(t) => out.push((t.pos, t.galley.text().to_string(), t.override_text_color.unwrap_or(t.fallback_color))),
            _ => {}
        }
    }
    let row = h.query_all_by_role_and_label(egui::accesskit::Role::Button, label).next()?.rect();
    let mut found = Vec::new();
    h.output().shapes.iter().for_each(|clipped| texts(&clipped.shape, &mut found));
    // Shapes come in paint order: what the row paints beside its name comes right after the name,
    // which keeps out the text under the open menu.
    let name = found.iter().rposition(|(pos, text, _)| row.contains(*pos) && text == label).expect("the row paints its name");
    found.into_iter().skip(name + 1).find(|(pos, _, _)| row.contains(*pos)).map(|(_, text, color)| (text, color))
}

/// The accessible description of the version row named `label` (its date and size).
fn row_facts(h: &Harness<'static, Option<LauncherUi>>, label: &str) -> Option<String> {
    h.query_all_by_role_and_label(egui::accesskit::Role::Button, label).next()?.accesskit_node().description()
}

/// The launcher's own list in Settings sizes the launcher's download for this machine.
#[test]
fn the_launcher_version_list_sizes_the_launchers_download() {
    let mut r = release("open3d-launcher", "0.9.0", NOW);
    r.assets[0].sha256 = Some("0".repeat(64));
    let mut ev = events();
    ev.push(Event::Versions { app: open3d_launcher_core::catalog::LAUNCHER_ID.into(), result: Ok(vec![r]) });
    let (mut h, _) = harness_with(ev, ViewMode::Grid, egui::Theme::Dark, true, 1.0);
    h.run_steps(6);
    h.get_by_label("Settings").click();
    h.run_steps(4);
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Launcher version").click();
    h.run_steps(5);
    let facts = row_facts(&h, "v0.9.0 (latest release)").unwrap_or_default();
    assert!(facts.starts_with("Oct ") && facts.ends_with(", 2026 · 150 MB"), "{facts:?}");
    assert_eq!(painted_facts(&h, "v0.9.0 (latest release)").map(|(text, _)| text), Some(facts));
}

/// Drives the real launcher lifecycle (`logic` then `ui`) in one `Context::run_ui` pass, optionally
/// asking egui for a discarded second pass over the same input.
fn drive_launcher(launcher: &mut LauncherUi, ctx: &egui::Context, events: Vec<egui::Event>, time: f64, two_passes: bool) {
    let mut first = two_passes;
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1180.0, 1200.0))),
        time: Some(time),
        events,
        ..Default::default()
    };
    let _ = ctx.run_ui(input, |root| {
        if first {
            first = false;
            root.ctx().request_discard("test: force a second pass");
        }
        launcher.logic(&root.ctx().clone(), &mut NoHost);
        launcher.ui(root, &mut NoHost);
    });
}

/// Drives one pointer gesture as move, press and release over separate frames, optionally
/// asking for a discarded second pass on release.
fn click_steps(launcher: &mut LauncherUi, ctx: &egui::Context, at: egui::Pos2, time: f64, two_passes_on_release: bool) {
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    drive_launcher(launcher, ctx, vec![egui::Event::PointerMoved(at)], time, false);
    drive_launcher(launcher, ctx, vec![button(true)], time + 0.1, false);
    drive_launcher(launcher, ctx, vec![button(false)], time + 0.2, two_passes_on_release);
}

#[test]
/// A toast created by logic while no window ran is shown once on the first drawing pass and
/// expires relative to that pass.
fn a_job_finished_while_hidden_shows_one_fresh_toast_on_the_next_frame() {
    open3d_launcher_ui_egui::i18n::set_test_lang(open3d_launcher_ui_egui::i18n::Lang::EN);
    let sent = Rc::new(RefCell::new(Vec::new()));
    let backend = Fake {
        downloads: Default::default(),
        events: vec![Event::JobFinished { app: "phototool".into(), result: Ok("PhotoTool updated to 0.5.0".into()) }],
        sent: sent.clone(),
        manage: true,
        pictures: Rc::default(),
        asked: Vec::new(),
        clock: Rc::new(Cell::new(NOW)),
    };
    // Build the harness with no launcher first: its construction pass runs at time 0, before a test
    // can set `RawInput::time`, and would otherwise consume the hidden message.
    let mut attached = false;
    // Ask for a discarded second pass on the first drawing frame: the toast path must be
    // idempotent across passes, not just across steps.
    let mut discard_once = true;
    let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 1200.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if let Some(l) = state {
                if !attached {
                    attached = true;
                    l.attach(ui.ctx());
                }
                if discard_once {
                    discard_once = false;
                    ui.ctx().request_discard("test: discard the first drawing pass");
                }
                l.logic(&ui.ctx().clone(), &mut NoHost);
                l.ui(ui, &mut NoHost);
            }
        },
        None,
    );
    // The tray path: `logic` runs without any UI pass, with the input time of the last shown frame.
    let windowless = egui::Context::default();
    let mut launcher = LauncherUi::new(&windowless, Box::new(backend), Settings { notify: false, language: "en".into(), ..Settings::default() });
    launcher.logic(&windowless, &mut NoHost);
    *h.state_mut() = Some(launcher);
    // The visible window's clock is far ahead of the logic-only tick's stale input time. The toast
    // must be fresh on the first drawing pass and must expire relative to it.
    h.input_mut().time = Some(1000.0);
    h.run_steps(2);
    assert_eq!(h.query_all_by_label("PhotoTool updated to 0.5.0").count(), 1, "the hidden result is shown once");
    h.run_steps(2);
    assert_eq!(h.query_all_by_label("PhotoTool updated to 0.5.0").count(), 1, "a discarded/next pass must not duplicate it");
    // A stale expiry (old logic time + 4.5 s) would already have dropped it; at 1004.6 the real
    // 1000.0 + 4.5 deadline has passed, so the toast goes.
    h.input_mut().time = Some(1004.6);
    h.run_steps(2);
    assert!(h.query_by_label("PhotoTool updated to 0.5.0").is_none(), "the toast expires relative to the drawing pass");
}

#[test]
fn install_locations_survive_settings_save_and_older_settings_use_system_default() {
    let old: Settings = serde_json::from_str(r#"{"auto_update":true}"#).unwrap();
    assert!(old.auto_update);
    assert_eq!(old.install_locations, Default::default());
    let mut settings = old;
    settings.install_locations.root = Some("/Volumes/Open3D apps".into());
    settings.install_locations.per_app.insert("phototool".into(), "/Volumes/Photo".into());
    let restored: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
    assert_eq!(restored.install_locations.folder("phototool"), Some("/Volumes/Photo"));
    assert_eq!(restored.install_locations.folder("filmtool"), Some("/Volumes/Open3D apps"));
}

#[test]
fn the_launcher_update_waiting_for_other_installs_can_be_cancelled() {
    let mut ev = events();
    ev.push(Event::LauncherUpdate(release("open3d-launcher", "0.2.0", NOW)));
    ev.push(Event::Progress { app: "open3d-launcher".into(), phase: JobPhase::Waiting });
    let (mut h, sent) = harness_with(ev, ViewMode::Grid, egui::Theme::Dark, true, 1.0);
    h.run_steps(6);
    snapshot(&mut h, "grid-launcher-waiting-dark");
    h.get_by_label("Waiting for other installs…");
    sent.borrow_mut().clear();
    h.get_by_label("Cancel update").click();
    h.run_steps(2);
    assert_eq!(*sent.borrow(), [Command::Cancel { app: "open3d-launcher".into() }]);
}

#[test]
/// The launcher banner's Cancel crosses the action boundary once per visible frame.
fn the_launcher_banner_cancel_is_sent_once_across_discarded_passes() {
    let mut ev = events();
    ev.push(Event::LauncherUpdate(release("open3d-launcher", "0.2.0", NOW)));
    ev.push(Event::Progress { app: "open3d-launcher".into(), phase: JobPhase::Waiting });
    let (mut h, sent) = harness_with(ev, ViewMode::Grid, egui::Theme::Dark, true, 1.0);
    h.run_steps(6);
    let at = h.get_by_label("Cancel update").rect().center();
    let ctx = h.ctx.clone();
    let mut launcher = h.state_mut().take().expect("the harness built the launcher");
    sent.borrow_mut().clear();
    click_steps(&mut launcher, &ctx, at, 100.0, true);
    let cancels = sent.borrow().iter().filter(|c| matches!(c, Command::Cancel { .. })).count();
    assert_eq!(cancels, 1, "the banner's Cancel must cross the action boundary once: {:?}", sent.borrow());
}

#[test]
/// Escape during a pin drag restores the order from drag start (live hover already mutated pins).
fn escape_cancels_pin_drag_and_restores_order() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    {
        let ui = h.state_mut().as_mut().unwrap();
        ui.settings.pin_app("phototool");
        ui.settings.pin_app("filmtool");
        ui.settings.pin_app("wordtool");
        assert_eq!(ui.settings.pinned, ["phototool", "filmtool", "wordtool"]);
        ui.begin_pin_drag_for_test("phototool");
        // Mid-drag: pointer has already reordered via drop targets.
        ui.settings.pinned = vec!["filmtool".into(), "phototool".into(), "wordtool".into()];
    }
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    let ui = h.state().as_ref().unwrap();
    assert!(!ui.pin_dragging_for_test());
    assert_eq!(ui.settings.pinned, ["phototool", "filmtool", "wordtool"]);
}

#[test]
/// Escape still restores pin order when search hides every app (grid/table never run).
fn escape_cancels_pin_drag_with_empty_results() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    {
        let ui = h.state_mut().as_mut().unwrap();
        ui.settings.pin_app("phototool");
        ui.settings.pin_app("filmtool");
        ui.begin_pin_drag_for_test("phototool");
        ui.settings.pinned = vec!["filmtool".into(), "phototool".into()];
        // Apply search without an intervening empty frame (pointer-up would end the drag).
        ui.set_search_for_test("zzzz-no-match");
        assert!(ui.visible_ids_for_test().is_empty());
        assert!(ui.pin_dragging_for_test());
    }
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    let ui = h.state().as_ref().unwrap();
    assert!(!ui.pin_dragging_for_test());
    assert_eq!(ui.settings.pinned, ["phototool", "filmtool"]);
}

/// The first close to the tray is explained once, by a system notification (#45, #215).
#[test]
fn the_first_close_to_the_tray_is_explained_once() {
    struct Notices(Vec<(String, String)>);
    impl Host for Notices {
        fn notify(&mut self, title: &str, body: &str) {
            self.0.push((title.to_string(), body.to_string()));
        }
    }
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    let mut host = Notices(Vec::new());
    let l = h.state_mut().as_mut().unwrap();
    assert!(!l.settings.tray_hint_shown);
    l.hidden_to_tray(&mut host);
    l.hidden_to_tray(&mut host);
    assert!(l.settings.tray_hint_shown);
    assert_eq!(host.0.len(), 1, "{:?}", host.0);
    assert_eq!(host.0[0].0, "Open3D Launcher is still running");
    assert!(host.0[0].1.contains("tray icon"), "{}", host.0[0].1);
}

#[test]
fn community_visibility_preference_round_trips_and_defaults_off_for_old_settings() {
    let old: Settings = serde_json::from_str("{}").unwrap();
    assert!(!old.hide_community_apps);
    let settings = Settings { hide_community_apps: true, ..old };
    let restored: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
    assert!(restored.hide_community_apps);
}

#[test]
/// Background app detection remains on its own five-minute cadence; release polling is separate.
fn looks_again_at_installed_apps_every_few_minutes_not_every_frame() {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let clock = Rc::new(Cell::new(NOW));
    let mut backend = Some(Fake {
        events: events(),
        sent: sent.clone(),
        manage: true,
        pictures: Rc::default(),
        asked: Vec::new(),
        downloads: Default::default(),
        clock: clock.clone(),
    });
    let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 1200.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), Settings { notify: false, ..Settings::default() }));
            }
            if let Some(l) = state.as_mut() {
                l.logic(&ui.ctx().clone(), &mut NoHost);
                l.ui(ui, &mut NoHost);
            }
        },
        None,
    );
    let detects = |sent: &Rc<RefCell<Vec<Command>>>| sent.borrow().iter().filter(|c| **c == Command::Detect).count();
    h.run_steps(10);
    assert_eq!(detects(&sent), 0, "the backend's own startup detection is enough");
    clock.set(NOW + 4 * 60);
    h.run_steps(10);
    assert_eq!(detects(&sent), 0, "not before five minutes");
    clock.set(NOW + 5 * 60);
    h.run_steps(10);
    assert_eq!(detects(&sent), 1, "once at five minutes, not once per frame");
}

#[test]
/// Update and restart queues `UpdateLauncher`, progress hides the button, and
/// `LauncherRestarting` asks the host to quit; the update title is announced once.
fn the_launcher_updates_itself_and_quits_to_restart() {
    struct QuitHost(Rc<Cell<bool>>);
    impl Host for QuitHost {
        fn quit(&mut self) {
            self.0.set(true);
        }
    }
    let sent = Rc::new(RefCell::new(Vec::new()));
    let mut events = events();
    events.push(Event::LauncherUpdate(release("open3d-launcher", "0.2.0", NOW)));
    let shared_events = Rc::new(RefCell::new(Vec::new()));
    struct Scripted {
        inner: Fake,
        later: Rc<RefCell<Vec<Event>>>,
    }
    impl Backend for Scripted {
        fn send(&mut self, c: Command) {
            self.inner.send(c);
        }
        fn poll(&mut self) -> Vec<Event> {
            let mut out = self.inner.poll();
            out.extend(self.later.borrow_mut().drain(..));
            out
        }
        fn capabilities(&self) -> Capabilities {
            self.inner.capabilities()
        }
        fn platform(&self) -> Option<Platform> {
            self.inner.platform()
        }
        fn now(&self) -> i64 {
            self.inner.now()
        }
    }
    let quit = Rc::new(Cell::new(false));
    let mut host = QuitHost(quit.clone());
    let mut backend = Some(Scripted {
        inner: Fake {
            events,
            sent: sent.clone(),
            manage: true,
            pictures: Rc::default(),
            asked: Vec::new(),
            downloads: Default::default(),
            clock: Rc::new(Cell::new(NOW)),
        },
        later: shared_events.clone(),
    });
    let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 1200.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), Settings { notify: false, ..Settings::default() }));
            }
            if let Some(l) = state.as_mut() {
                l.logic(&ui.ctx().clone(), &mut host);
                l.ui(ui, &mut host);
            }
        },
        None,
    );
    h.run_steps(6);
    h.get_by_role_and_label(egui::accesskit::Role::Label, "Open3D Launcher 0.2.0 is available.");
    assert_eq!(
        h.query_all_by_label_contains("Open3D Launcher 0.2.0 is available").count(),
        1,
        "the update title is announced once, not once by the icon and once by the heading"
    );
    h.get_by_label("Update and restart").click();
    h.run_steps(2);
    assert!(sent.borrow().contains(&Command::UpdateLauncher));
    // Downloading: the banner shows progress instead of the button.
    shared_events
        .borrow_mut()
        .push(Event::Progress { app: "open3d-launcher".into(), phase: JobPhase::Downloading { done: 5 << 20, total: 10 << 20 } });
    h.run_steps(3);
    assert!(h.query_by_label("Update and restart").is_none());
    assert!(!quit.get());
    // In place: the launcher quits so the helper can start the new one.
    shared_events.borrow_mut().push(Event::LauncherRestarting);
    h.run_steps(3);
    assert!(quit.get());
}

#[test]
/// A found launcher update opens the relaunch prompt once; Later dismisses it and leaves the
/// toolbar banner.
fn launcher_update_prompts_to_relaunch_with_updated_version() {
    let mut ev = events();
    ev.push(Event::LauncherUpdate(release("open3d-launcher", "0.2.0", NOW)));
    let (mut h, _) = harness_with(ev, ViewMode::Grid, egui::Theme::Dark, true, 1.0);
    h.run_steps(6);
    // Dialog pops up on launch with the update prompt
    h.get_by_role_and_label(egui::accesskit::Role::Label, "Open3D Launcher 0.2.0 is available.");
    assert_eq!(
        h.query_all_by_label_contains("Open3D Launcher 0.2.0 is available").count(),
        1,
        "the update title is announced once, not once by the icon and once by the heading"
    );
    h.get_by_label("A new version is available. Would you like to relaunch with the updated version?");
    h.get_by_label("What's new");
    // Clicking "Later" dismisses the modal dialog
    h.get_by_label("Later").click();
    h.run_steps(3);
    assert!(h.query_by_label("A new version is available. Would you like to relaunch with the updated version?").is_none());
    // Toolbar update banner is now visible with Update and restart
    h.get_by_label("Update and restart");
}

#[test]
fn the_current_desktop_setting_is_only_offered_where_the_os_has_desktops() {
    // NoHost has no desktops (Windows, Linux, web): no setting.
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    h.get_by_label("Settings").click();
    h.run_steps(4);
    assert!(h.query_by_label("Show the window on the current desktop").is_none());
    // On by default for everyone; macOS shows it in Settings (DesktopHost::has_desktops).
    assert!(Settings::default().show_on_current_desktop);
}

#[test]
/// A feed alert is acknowledged only by OK (persisted), while an inline announcement stays until
/// its own Dismiss is clicked.
fn feed_announcements_and_alerts_show_until_dismissed() {
    use open3d_launcher_core::feed::Notice;
    let notice = |id: &str, title: &str, link: Option<&str>| Notice {
        id: id.into(),
        title: title.into(),
        body: format!("{title} body"),
        link: link.map(str::to_string),
        created_at: None,
    };
    let sent = Rc::new(RefCell::new(Vec::new()));
    let mut evs = events();
    evs.push(Event::Notices {
        announcements: vec![notice("a1", "CADTool 1.0 is out", Some("https://github.com/carlesllavata/Open3D/news"))],
        alerts: vec![notice("x1", "Security update", Some("https://github.com/carlesllavata/Open3D/security"))],
    });
    let mut backend = Some(Fake {
        events: evs,
        sent,
        manage: true,
        pictures: Rc::default(),
        asked: Vec::new(),
        downloads: Default::default(),
        clock: Rc::new(Cell::new(NOW)),
    });
    let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 1200.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), Settings { notify: false, ..Settings::default() }));
            }
            if let Some(l) = state.as_mut() {
                l.logic(&ui.ctx().clone(), &mut NoHost);
                l.ui(ui, &mut NoHost);
            }
        },
        None,
    );
    h.run_steps(6);
    // The alert pops up first; OK acknowledges it for good.
    h.get_by_label("Security update");
    h.get_by_label("Open link");
    h.get_by_label("OK").click();
    h.run_steps(4);
    assert!(h.query_by_label("Security update").is_none());
    assert!(h.state().as_ref().unwrap().settings.dismissed_notices.contains("x1"));
    // The announcement stays inline until dismissed.
    h.get_by_label("CADTool 1.0 is out");
    h.get_by_label("Learn more");
    h.get_all_by_label("Dismiss").next().unwrap().click();
    h.run_steps(4);
    assert!(h.query_by_label("CADTool 1.0 is out").is_none());
    assert!(h.state().as_ref().unwrap().settings.dismissed_notices.contains("a1"));
}

#[test]
fn automatic_checks_run_every_twelve_minutes() {
    use open3d_launcher_ui_egui::settings::CheckEvery;
    assert_eq!(Settings::default().check_every.seconds(), Some(720));
    // Older saved choices still load, and mean "automatically" now.
    assert_eq!(CheckEvery::Day.seconds(), Some(720));
    assert_eq!(CheckEvery::Never.seconds(), None);
}

#[test]
/// With auto-update off, no app install or launcher update is queued.
fn automatic_updates_are_disabled_by_default() {
    let mut ev = events();
    ev.push(Event::LauncherUpdate(release("open3d-launcher", "0.2.0", NOW)));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let sent2 = sent.clone();
    let mut backend = Some(Fake {
        events: ev,
        sent: sent2,
        manage: true,
        pictures: Rc::default(),
        asked: Vec::new(),
        downloads: Default::default(),
        clock: Rc::new(Cell::new(NOW)),
    });
    let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 1200.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), Settings { auto_update: false, notify: false, ..Settings::default() }));
            }
            if let Some(l) = state.as_mut() {
                l.logic(&ui.ctx().clone(), &mut NoHost);
                l.ui(ui, &mut NoHost);
            }
        },
        None,
    );
    h.run_steps(4);
    let commands = sent.borrow().clone();
    assert!(!commands.contains(&Command::Install { app: "phototool".into() }), "phototool should not be auto-installed when auto_update is false");
    assert!(!commands.contains(&Command::UpdateLauncher), "launcher should not be auto-updated when auto_update is false");
}

#[test]
/// A custom title bar shows Minimize, Maximize and Close, and Close requests the viewport close
/// command.
fn custom_title_bar_has_window_buttons() {
    struct FramelessHost;
    impl Host for FramelessHost {
        fn title_bar(&self) -> open3d_launcher_ui_egui::TitleBar {
            open3d_launcher_ui_egui::TitleBar::Custom
        }
    }
    let mut backend = Some(Fake {
        events: events(),
        sent: Rc::new(RefCell::new(Vec::new())),
        manage: true,
        pictures: Rc::default(),
        asked: Vec::new(),
        downloads: Default::default(),
        clock: Rc::new(Cell::new(NOW)),
    });
    let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 1200.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), Settings { notify: false, ..Settings::default() }));
            }
            if let Some(l) = state.as_mut() {
                l.logic(&ui.ctx().clone(), &mut FramelessHost);
                l.ui(ui, &mut FramelessHost);
            }
        },
        None,
    );
    h.run_steps(6);
    snapshot(&mut h, "grid-title-bar-dark");
    h.get_by_label("Minimize");
    h.get_by_label("Maximize");
    h.get_by_label("Close").click();
    h.step();
    let closes = h.output().viewport_output.values().any(|v| v.commands.contains(&egui::ViewportCommand::Close));
    assert!(closes, "the close button asks the window to close (the shell decides whether that hides it to the tray)");
}

/// Maximized, the window's top-right pixel is the screen's corner: there is no resize band there,
/// so it belongs to the close button, as it does with Windows' own caption buttons (#374).
#[test]
fn maximized_the_close_button_reaches_the_screens_corner() {
    let (mut h, _) = harness_with_title_bar(events(), ViewMode::Grid, egui::Theme::Dark, true, 1.0, open3d_launcher_ui_egui::TitleBar::Custom);
    h.run_steps(6);
    let set_maximized = |h: &mut Harness<'_, Option<LauncherUi>>, maximized: bool| {
        h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().maximized = Some(maximized);
        h.run_steps(2);
    };
    // Presses and releases at `pos`, and returns what the window was asked to do.
    let click = |h: &mut Harness<'_, Option<LauncherUi>>, pos: egui::Pos2| -> Vec<egui::ViewportCommand> {
        let mut asked = Vec::new();
        for pressed in [true, false] {
            let input = h.input_mut();
            input.events.push(egui::Event::PointerMoved(pos));
            input.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            h.step();
            asked.extend(h.output().viewport_output.values().flat_map(|v| v.commands.iter().cloned()));
        }
        asked
    };
    // Windowed, the clickable area stops short of the button's top-right corner by the resize
    // band; maximized, it reaches it.
    let windowed = h.get_by_label("Close").rect();
    set_maximized(&mut h, true);
    let maximized = h.get_by_label("Close").rect();
    assert!(maximized.top() < windowed.top() && maximized.right() > windowed.right(), "windowed {windowed:?}, maximized {maximized:?}");
    // The button's own corner stands in for the screen's: the harness keeps the app 8 px in from
    // the window. Windowed, it is beyond the band and egui's click tolerance; maximized, it closes.
    let corner = egui::pos2(maximized.right() - 0.5, maximized.top() + 0.5);
    let asked = click(&mut h, corner);
    assert!(asked.contains(&egui::ViewportCommand::Close), "maximized: {asked:?}");
    set_maximized(&mut h, false);
    let asked = click(&mut h, corner);
    assert!(!asked.contains(&egui::ViewportCommand::Close), "windowed: {asked:?}");
    // And the window's own corner is still for resizing.
    let asked = click(&mut h, egui::pos2(1180.0 - 0.5, 0.5));
    assert!(asked.iter().any(|c| matches!(c, egui::ViewportCommand::BeginResize(_))), "{asked:?}");
}

/// A quick drag on an edge only resizes: egui sees it as one spot, so nothing nearby may click (#571).
#[test]
fn a_press_on_the_window_edge_only_resizes() {
    use egui::ViewportCommand as Asked;
    struct FramelessHost;
    impl Host for FramelessHost {
        fn title_bar(&self) -> open3d_launcher_ui_egui::TitleBar {
            open3d_launcher_ui_egui::TitleBar::Custom
        }
    }
    open3d_launcher_ui_egui::i18n::set_test_lang(open3d_launcher_ui_egui::i18n::Lang::EN);
    let mut backend = Some(Fake {
        downloads: Default::default(),
        events: events(),
        sent: Rc::new(RefCell::new(Vec::new())),
        manage: true,
        pictures: Rc::default(),
        asked: Vec::new(),
        clock: Rc::new(Cell::new(NOW)),
    });
    let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 1200.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), Settings { notify: false, ..Settings::default() }));
            }
            if let Some(l) = state.as_mut() {
                l.logic(&ui.ctx().clone(), &mut FramelessHost);
                // The harness keeps its app 8 px in; a real header starts at the window's top.
                let window = ui.ctx().content_rect();
                ui.scope_builder(egui::UiBuilder::new().max_rect(window), |ui| l.ui(ui, &mut FramelessHost));
            }
        },
        None,
    );
    h.run_steps(6);
    let press = |h: &mut Harness<'_, Option<LauncherUi>>, pos: egui::Pos2| -> Vec<Asked> {
        let mut asked = Vec::new();
        for pressed in [true, false] {
            let input = h.input_mut();
            input.events.push(egui::Event::PointerMoved(pos));
            input.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            h.step();
            asked.extend(h.output().viewport_output.values().flat_map(|v| v.commands.iter().cloned()));
        }
        h.run_steps(2);
        asked
    };
    let settings_open =
        |h: &Harness<'_, Option<LauncherUi>>| h.query_by_role_and_label(egui::accesskit::Role::ComboBox, "Launcher version").is_some();
    for label in ["Minimize", "Maximize", "Close", "Settings"] {
        let x = h.get_by_label(label).rect().center().x;
        let asked = press(&mut h, egui::pos2(x, 2.0));
        assert!(asked.iter().any(|c| matches!(c, Asked::BeginResize(_))), "{label}: {asked:?}");
        assert!(!asked.iter().any(|c| matches!(c, Asked::Minimized(_) | Asked::Maximized(_) | Asked::Close)), "{label}: {asked:?}");
    }
    assert!(!settings_open(&h), "the gear was pressed through the edge");
    // The first frame at a new size already covers the new edge, or Close slips out from under it.
    h.set_size(egui::vec2(1300.0, 1200.0));
    h.step();
    let x = h.get_by_label("Close").rect().center().x;
    let asked = press(&mut h, egui::pos2(x, 2.0));
    assert!(!asked.iter().any(|c| matches!(c, Asked::Close)), "after growing: {asked:?}");
    // The first frame after a restore has no edge areas to hit yet, only last frame's full buttons.
    let press_on_restore = |h: &mut Harness<'_, Option<LauncherUi>>, label: &str, y: f32| -> Vec<Asked> {
        h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().maximized = Some(true);
        h.run_steps(3);
        let x = h.get_by_label(label).rect().center().x;
        h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().maximized = Some(false);
        press(h, egui::pos2(x, y))
    };
    for label in ["Minimize", "Restore", "Close"] {
        let asked = press_on_restore(&mut h, label, 4.5);
        assert!(!asked.iter().any(|c| matches!(c, Asked::Minimized(_) | Asked::Maximized(_) | Asked::Close)), "{label} after restoring: {asked:?}");
    }
    // Just below the band, that frame's click is the button's.
    let asked = press_on_restore(&mut h, "Minimize", 5.5);
    assert!(asked.contains(&Asked::Minimized(true)), "below the band after restoring: {asked:?}");
    // A key press still works with the pointer resting on the band, and so does a click below it.
    let x = h.get_by_label("Minimize").rect().center().x;
    press(&mut h, egui::pos2(x, 4.5));
    h.get_by_label("Minimize").focus();
    h.run_steps(2);
    h.key_down(egui::Key::Enter);
    h.step();
    let asked: Vec<_> = h.output().viewport_output.values().flat_map(|v| v.commands.iter().cloned()).collect();
    assert!(asked.contains(&Asked::Minimized(true)), "by key: {asked:?}");
    h.key_up(egui::Key::Enter);
    h.step();
    for (label, want) in [("Minimize", Asked::Minimized(true)), ("Maximize", Asked::Maximized(true)), ("Close", Asked::Close)] {
        let x = h.get_by_label(label).rect().center().x;
        let asked = press(&mut h, egui::pos2(x, 5.5));
        assert!(asked.contains(&want), "{label} below the band: {asked:?}");
        assert!(!asked.iter().any(|c| matches!(c, Asked::BeginResize(_))), "{label} below the band: {asked:?}");
    }
    // A dialog's backdrop reaches the edges too; the press doesn't dismiss it.
    h.get_by_label("Settings").click();
    h.run_steps(4);
    assert!(settings_open(&h));
    let asked = press(&mut h, egui::pos2(590.0, 1198.0));
    assert!(asked.iter().any(|c| matches!(c, Asked::BeginResize(_))), "{asked:?}");
    assert!(settings_open(&h), "the dialog closed on a resize");
}

/// Every page, whatever it holds: no pictures (PdfTool), one (DesignTool) or several. The arrow keys are
/// pressed more often than there are pictures, both ways, to wrap around the list and to do nothing on a
/// page that has one or none; the enlarged view is opened wherever there is a picture to enlarge.
#[test]
fn every_page_opens_and_pages_through_its_pictures() {
    let (mut h, _sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    // Tall enough to show all thirteen cards at once.
    h.set_size(egui::vec2(1180.0, 3000.0));
    h.run_steps(8);
    for d in &open3d_launcher_core::details::DETAILS {
        let name = SUITE.iter().find(|a| a.id == d.id).map(|a| a.name).unwrap_or(d.id);
        h.get_by_label(&format!("{name} details")).scroll_to_me();
        h.run_steps(3);
        h.get_by_label(&format!("{name} details")).click();
        h.run_steps(8);
        h.get_all_by_label_contains(d.headline).last().unwrap();
        let presses = d.shots.len() + 2;
        for key in [egui::Key::ArrowRight, egui::Key::ArrowLeft] {
            for _ in 0..presses {
                h.key_press(key);
                h.run_steps(1);
            }
        }
        // The picture can be enlarged exactly when the page has one.
        assert_eq!(h.query_by_label("Enlarge picture").is_some(), !d.shots.is_empty(), "{}", d.id);
        if !d.shots.is_empty() {
            h.get_by_label("Enlarge picture").click();
            h.run_steps(3);
            for key in [egui::Key::ArrowRight, egui::Key::ArrowLeft] {
                for _ in 0..presses {
                    h.key_press(key);
                    h.run_steps(1);
                }
            }
            // A full lap in each direction brings it back to the first picture. Its caption is in the drawer
            // and in the enlarged view, and has a counter only when there are several pictures.
            let first = &d.shots[0];
            let caption = if d.shots.len() > 1 { format!("{} · 1/{}", first.caption, d.shots.len()) } else { first.caption.to_string() };
            assert!(h.query_all_by_label_contains(&caption).count() >= 1, "{}: {caption}", d.id);
            h.key_press(egui::Key::Escape);
            h.run_steps(3);
        }
        h.key_press(egui::Key::Escape);
        h.run_steps(10);
        // The card keeps its own copy of the line; the page's is gone.
        assert!(h.query_all_by_label_contains(d.headline).count() <= 1, "{} did not close", d.id);
    }
}

#[test]
/// At 720–1180 px refresh, search, sort and view share one unclipped toolbar row without
/// overlaps.
fn toolbar_keeps_navigation_refresh_search_and_view_on_one_unclipped_row() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    for width in [720.0, 800.0, 1180.0] {
        h.set_size(egui::vec2(width, 900.0));
        h.run_steps(6);
        let refresh = h.get_by_label("Check for updates").rect();
        let search = h.query_all_by_role(egui::accesskit::Role::TextInput).next().unwrap().rect();
        let view = h.get_by_label("Show apps as cards").rect();
        let sort = h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Sort").rect();
        assert!(refresh.top() > 48.0, "{width}: refresh belongs in the toolbar, not the header: {refresh:?}");
        assert!((refresh.center().y - search.center().y).abs() < 2.0, "{width}: refresh and search must share a row");
        assert!(refresh.right() <= search.left() + 1.0, "{width}: refresh overlaps search: {refresh:?}, {search:?}");
        assert!(refresh.right() <= sort.left() + 1.0, "{width}: sort overlaps refresh: {refresh:?}, {sort:?}");
        assert!(sort.right() <= search.left() + 1.0, "{width}: sort overlaps search: {sort:?}, {search:?}");
        assert!(search.right() <= view.left() + 1.0 && view.right() <= width, "{width}: search overlaps view or view is clipped");
        for (label, rect) in [("refresh", refresh), ("search", search), ("sort", sort), ("view", view)] {
            assert!(rect.left() >= 0.0 && rect.right() <= width, "{width}: {label} is outside the window: {rect:?}");
        }
        assert!(!sort.intersects(search) && !view.intersects(search), "{width}: toolbar controls overlap");
        h.check_accessibility();
    }
}

#[test]
fn the_toolbar_has_no_filter_dropdown() {
    use open3d_launcher_ui_egui::settings::Filter;
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    for label in ["All 13", "Installed 4", "Updates 2", "Not installed 9"] {
        assert!(h.query_by_value(label).is_none() && h.query_by_label(label).is_none(), "{label}");
    }
    // Only the sort dropdown is left.
    assert_eq!(h.query_all_by_role(egui::accesskit::Role::ComboBox).count(), 1);
    assert_eq!(h.state().as_ref().unwrap().settings.filter, Filter::All);
}

#[test]
fn saved_filters_are_migrated_on_ui_construction() {
    use open3d_launcher_ui_egui::settings::Filter;

    for (saved, expected) in [
        (r#"{"filter":"All"}"#, Filter::All),
        (r#"{"filter":"Installed"}"#, Filter::All),
        (r#"{"filter":"Updates"}"#, Filter::All),
        (r#"{"filter":"NotInstalled"}"#, Filter::All),
        (r#"{"filter":"Hidden"}"#, Filter::Hidden),
    ] {
        let mut settings: Settings = serde_json::from_str(saved).unwrap();
        settings.language = "en".into(); // Avoid the global system-locale probe in parallel tests.
        settings.last_check = Some(NOW);
        let backend = Fake {
            events: Vec::new(),
            sent: Rc::default(),
            manage: true,
            pictures: Rc::default(),
            asked: Vec::new(),
            downloads: Default::default(),
            clock: Rc::new(Cell::new(NOW)),
        };
        let ui = LauncherUi::new(&egui::Context::default(), Box::new(backend), settings);
        assert_eq!(ui.settings.filter, expected, "{saved}");
        assert_eq!(ui.settings.last_check, Some(NOW));
    }
}

/// Checks that, at one window width, every header control stays inside the window and clear of
/// the brand text, that a custom title bar's buttons are there and inside, and snapshots the
/// three widths that matter visually.
fn assert_header_at_width(h: &mut Harness<'_, Option<LauncherUi>>, bar: open3d_launcher_ui_egui::TitleBar, theme: egui::Theme, width: f32) {
    for label in ["Show apps as cards", "Show apps as a table", "Settings", "Check for updates"] {
        let rect = h.get_by_label(label).rect();
        assert!(rect.left() >= 0.0 && rect.right() <= width, "{bar:?} {width}: {label} {rect:?}");
        if let Some(text) = h.query_by_label("Open3D suite") {
            assert!(!rect.intersects(text.rect()), "{bar:?} {width}: Open3D suite overlaps {label} {rect:?}");
        }
    }
    if bar == open3d_launcher_ui_egui::TitleBar::Custom {
        for label in ["Minimize", "Maximize", "Close"] {
            let rect = h.get_by_label(label).rect();
            assert!(rect.left() >= 0.0 && rect.right() <= width, "{width}: {label} {rect:?}");
        }
    }
    if width == 1180.0 {
        assert!(h.query_by_label("Launcher").is_none());
        h.get_by_label("Open3D suite");
    }
    if [720.0, 800.0, 1180.0].contains(&width) {
        snapshot(h, &format!("header-{theme:?}-{bar:?}-{width}"));
    }
}

#[test]
fn narrow_header_keeps_the_view_switch_clear_of_brand_text() {
    use open3d_launcher_ui_egui::TitleBar;
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        for bar in [TitleBar::Native, TitleBar::MacOs, TitleBar::Custom] {
            let (mut h, _) = harness_with_title_bar(events(), ViewMode::Grid, theme, true, 1.0, bar);
            for width in (720..=1180).step_by(10).chain([720, 1180]).map(|w| w as f32) {
                h.set_size(egui::vec2(width, 900.0));
                h.run_steps(6);
                assert_header_at_width(&mut h, bar, theme, width);
            }
            h.get_by_label("Show apps as a table").click();
            h.run_steps(2);
            assert_eq!(h.state().as_ref().unwrap().settings.view, ViewMode::Table);
        }
    }
}

/// A shell on an OS whose animation setting the test switches.
struct OsMotion(Rc<Cell<bool>>);

impl Host for OsMotion {
    fn reduce_motion(&self) -> bool {
        self.0.get()
    }
}

/// The grid in a window too short for it, at 60 frames a second, on an OS asking for fewer
/// animations (`reduce`) or not. Nothing is downloading, so the window is still unless something
/// animates.
fn short_window(reduce: Rc<Cell<bool>>) -> Harness<'static, Option<LauncherUi>> {
    open3d_launcher_ui_egui::i18n::set_test_lang(open3d_launcher_ui_egui::i18n::Lang::EN);
    let events = events().into_iter().filter(|e| !matches!(e, Event::Progress { .. })).collect();
    let mut backend = Some(Fake {
        events,
        sent: Rc::default(),
        manage: true,
        pictures: Rc::default(),
        asked: Vec::new(),
        downloads: Default::default(),
        clock: Rc::new(Cell::new(NOW)),
    });
    let mut host = OsMotion(reduce);
    let mut h =
        test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 500.0)).with_step_dt(1.0 / 60.0).with_max_steps(120).build_ui_state(
            move |ui, state: &mut Option<LauncherUi>| {
                if state.is_none()
                    && let Some(b) = backend.take()
                {
                    let settings = Settings { view: ViewMode::Grid, notify: false, ..Settings::default() };
                    *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), settings));
                }
                if let Some(l) = state.as_mut() {
                    l.logic(&ui.ctx().clone(), &mut host);
                    l.ui(ui, &mut host);
                }
            },
            None,
        );
    h.run_steps(8);
    h
}

#[test]
fn the_os_animation_setting_is_followed_as_it_changes() {
    let reduce = Rc::new(Cell::new(true));
    let mut h = short_window(reduce.clone());
    assert_eq!(h.ctx.global_style().animation_time, 0.0);
    // Switched back on while the launcher runs: noticed within a couple of seconds.
    reduce.set(false);
    h.run_steps(150);
    assert_eq!(h.ctx.global_style().animation_time, egui::Style::default().animation_time);
    reduce.set(true);
    h.run_steps(150);
    assert_eq!(h.ctx.global_style().animation_time, 0.0);
}

#[test]
/// Command-plus-wheel over the main UI raises the global zoom factor.
fn control_wheel_zooms_the_main_ui() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(4);
    let before = h.ctx.zoom_factor();
    h.input_mut().events.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        phase: egui::TouchPhase::Move,
        delta: egui::vec2(0.0, 100.0),
        modifiers: egui::Modifiers::COMMAND,
        source: egui::MouseWheelSource::Unknown,
    });
    h.step();
    h.run_steps(2);
    assert!(h.ctx.zoom_factor() > before);
}

#[test]
fn command_m_minimizes_the_mac_window_only() {
    use open3d_launcher_ui_egui::TitleBar;
    for bar in [TitleBar::MacOs, TitleBar::Native, TitleBar::Custom] {
        let (mut h, _) = harness_with_title_bar(events(), ViewMode::Grid, egui::Theme::Dark, true, 1.0, bar);
        h.run_steps(4);
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::M,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::MAC_CMD,
        });
        h.step();
        let minimized = h.output().viewport_output.values().any(|v| v.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Minimized(true))));
        assert_eq!(minimized, bar == TitleBar::MacOs, "{bar:?}");
    }
}

#[test]
fn the_sort_offers_name_both_ways_recently_updated_and_frequently_used() {
    use open3d_launcher_ui_egui::settings::SortKey;
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    h.get_by_value("Name (A–Z)").click();
    h.run_steps(3);
    for label in ["Name (A–Z)", "Name (Z–A)", "Recently updated", "Frequently used"] {
        h.get_by_label(label);
    }
    h.get_by_label("Name (Z–A)").click();
    h.run_steps(4);
    let s = &h.state().as_ref().unwrap().settings;
    assert_eq!((s.sort, s.sort_descending), (SortKey::Name, true));
    h.get_by_value("Name (Z–A)").click();
    h.run_steps(3);
    h.get_by_label("Frequently used").click();
    h.run_steps(4);
    assert_eq!(h.state().as_ref().unwrap().settings.sort, SortKey::Frequent);
}
