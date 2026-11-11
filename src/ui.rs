// SPDX-License-Identifier: GPL-3.0-or-later
//! The launcher window. All slow work (catalog, downloads, file moves, checking for 3ds Max) runs on
//! background threads that report back through a channel.

use crate::catalog::{self, AppEntry, Catalog, Release};
use crate::{ICON_PNG, Options, StartMode, install, net, notes, setup};
use eframe::egui::{self, Color32, RichText};
use std::cmp::Ordering;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What the window shows.
#[derive(Clone, PartialEq, Eq)]
enum Screen {
    /// First run of a downloaded copy: install the launcher or just run it.
    Welcome,
    /// The app list.
    Main,
    /// `--uninstall`: confirm removing the launcher.
    Uninstall,
    /// A final message, with a Close button.
    Done(String),
}

const ACCENT: Color32 = Color32::from_rgb(255, 140, 26);
const OK: Color32 = Color32::from_rgb(110, 200, 120);
const WARN: Color32 = Color32::from_rgb(240, 190, 80);
const ERR: Color32 = Color32::from_rgb(235, 100, 90);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verb {
    Install,
    Update,
    Reinstall,
    Uninstall,
}

impl Verb {
    fn doing(self) -> &'static str {
        match self {
            Verb::Install => "Installing",
            Verb::Update => "Updating",
            Verb::Reinstall => "Reinstalling",
            Verb::Uninstall => "Removing",
        }
    }
    fn done(self) -> &'static str {
        match self {
            Verb::Install => "installed",
            Verb::Update => "updated",
            Verb::Reinstall => "reinstalled",
            Verb::Uninstall => "removed",
        }
    }
}

enum Msg {
    Catalog(Result<Catalog, String>),
    MaxRunning(bool),
    Progress {
        done: u64,
        total: Option<u64>,
    },
    Finished {
        name: String,
        verb: Verb,
        result: Result<(), String>,
    },
    /// The launcher installed itself; the path is the installed exe.
    SelfInstalled(Result<PathBuf, String>),
    SelfUninstalled(Result<(), String>),
    /// A newer launcher replaced this exe; the path is the exe to start.
    LauncherUpdated(Result<PathBuf, String>),
}

struct Job {
    name: String,
    verb: Verb,
    done: u64,
    total: Option<u64>,
}

#[derive(Default)]
struct AppState {
    installed: Option<String>,
    other_copies: Vec<String>,
}

pub struct Launcher {
    opts: Options,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    catalog: Option<Catalog>,
    loading: bool,
    catalog_error: Option<String>,
    states: Vec<AppState>,
    selected: usize,
    max_running: bool,
    job: Option<Job>,
    message: Option<(bool, String)>,
    confirm_uninstall: Option<String>,
    screen: Screen,
    desktop_shortcut: bool,
    /// Installing / removing the launcher itself, or updating it.
    setup_busy: bool,
    setup_error: Option<String>,
    icon: Option<egui::TextureHandle>,
}

impl Launcher {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Options) -> Self {
        let screen = match opts.mode {
            StartMode::Uninstall => Screen::Uninstall,
            StartMode::Normal if !setup::is_installed_copy() => Screen::Welcome,
            _ => Screen::Main,
        };
        let icon = eframe::icon_data::from_png_bytes(ICON_PNG).ok().map(|d| {
            let image =
                egui::ColorImage::from_rgba_unmultiplied([d.width as usize, d.height as usize], &d.rgba);
            cc.egui_ctx
                .load_texture("open3d-icon", image, egui::TextureOptions::LINEAR)
        });
        let ctx = cc.egui_ctx.clone();
        ctx.set_theme(egui::Theme::Dark);
        ctx.style_mut_of(egui::Theme::Dark, |s| {
            s.visuals.selection.bg_fill = ACCENT.linear_multiply(0.6);
            s.visuals.hyperlink_color = ACCENT;
            s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        });
        let (tx, rx) = channel();

        // Watch for 3ds Max every few seconds.
        let watch_tx = tx.clone();
        let watch_ctx = ctx.clone();
        std::thread::spawn(move || {
            loop {
                if watch_tx.send(Msg::MaxRunning(install::max_running())).is_err() {
                    break;
                }
                watch_ctx.request_repaint();
                std::thread::sleep(Duration::from_secs(3));
            }
        });

        let mut me = Launcher {
            opts,
            tx,
            rx,
            catalog: None,
            loading: false,
            catalog_error: None,
            states: Vec::new(),
            selected: 0,
            max_running: false,
            job: None,
            message: None,
            confirm_uninstall: None,
            screen,
            desktop_shortcut: true,
            setup_busy: false,
            setup_error: None,
            icon,
        };
        if me.screen != Screen::Uninstall {
            me.load_catalog(&ctx);
        }
        me
    }

    fn spawn_setup(&mut self, ctx: &egui::Context, work: impl FnOnce() -> Msg + Send + 'static) {
        self.setup_busy = true;
        self.setup_error = None;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work());
            ctx.request_repaint();
        });
    }

    /// The catalog's launcher release, when it is newer than this exe.
    fn launcher_update(&self) -> Option<Release> {
        let l = self.catalog.as_ref()?.launcher.as_ref()?;
        (catalog::compare_versions(&l.version, VERSION) == Ordering::Greater).then(|| l.clone())
    }

    fn start_launcher_update(&mut self, ctx: &egui::Context, rel: Release) {
        if self.job.is_some() || self.setup_busy {
            return;
        }
        self.message = None;
        self.job = Some(Job {
            name: "Open3D Launcher".into(),
            verb: Verb::Update,
            done: 0,
            total: None,
        });
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let loc = self.opts.catalog.join(&rel.url);
        std::thread::spawn(move || {
            let result = (|| {
                let dest = setup::download_path()?;
                let mut last = Instant::now() - Duration::from_secs(1);
                let hash = net::download(&loc, &dest, |done, total| {
                    if last.elapsed() > Duration::from_millis(80) || Some(done) == total {
                        last = Instant::now();
                        let _ = tx.send(Msg::Progress { done, total });
                        ctx.request_repaint();
                    }
                });
                let checked = hash.and_then(|h| {
                    let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
                    if !h.eq_ignore_ascii_case(&rel.sha256) {
                        Err("The download is damaged (its checksum does not match the catalog).".to_string())
                    } else if rel.size > 0 && size != rel.size {
                        Err(format!(
                            "The download has {size} bytes, the catalog says {}.",
                            rel.size
                        ))
                    } else {
                        Ok(())
                    }
                });
                if let Err(e) = checked {
                    let _ = std::fs::remove_file(&dest);
                    return Err(e);
                }
                setup::replace_running(&dest, &rel.version)
            })();
            let _ = tx.send(Msg::LauncherUpdated(result));
            ctx.request_repaint();
        });
    }

    fn load_catalog(&mut self, ctx: &egui::Context) {
        self.loading = true;
        self.catalog_error = None;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let loc = self.opts.catalog.clone();
        std::thread::spawn(move || {
            let result = net::read_catalog(&loc).and_then(|t| catalog::parse(&t));
            let _ = tx.send(Msg::Catalog(result));
            ctx.request_repaint();
        });
    }

    fn refresh_states(&mut self) {
        let Some(cat) = &self.catalog else { return };
        let mut problems = Vec::new();
        self.states = cat
            .apps
            .iter()
            .map(|a| {
                if let Err(e) = install::recover(&self.opts.plugins, a) {
                    problems.push(e);
                }
                AppState {
                    installed: install::installed_version(&self.opts.plugins, a),
                    other_copies: install::other_copies(&self.opts.plugins, a),
                }
            })
            .collect();
        if !problems.is_empty() {
            self.message = Some((true, problems.join("\n")));
        }
    }

    fn handle_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Catalog(Ok(c)) => {
                    self.loading = false;
                    self.selected = self.selected.min(c.apps.len().saturating_sub(1));
                    self.catalog = Some(c);
                    self.refresh_states();
                }
                Msg::Catalog(Err(e)) => {
                    self.loading = false;
                    self.catalog_error = Some(e);
                }
                Msg::MaxRunning(r) => self.max_running = r,
                Msg::Progress { done, total } => {
                    if let Some(j) = &mut self.job {
                        j.done = done;
                        j.total = total;
                    }
                }
                Msg::Finished { name, verb, result } => {
                    self.job = None;
                    self.message = Some(match result {
                        Ok(()) => (false, format!("{name} {}.", verb.done())),
                        Err(e) => (true, format!("{} {name} failed: {e}", verb.doing())),
                    });
                    self.refresh_states();
                }
                Msg::SelfInstalled(result) => {
                    self.setup_busy = false;
                    match result.and_then(|exe| setup::launch(&exe, false)) {
                        Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                        Err(e) => self.setup_error = Some(e),
                    }
                }
                Msg::SelfUninstalled(result) => {
                    self.setup_busy = false;
                    match result {
                        Ok(()) => self.screen = Screen::Done(
                            "Open3D Launcher was removed from this computer. The apps you installed with it \
                                 are still in 3ds Max."
                                .into(),
                        ),
                        Err(e) => self.setup_error = Some(e),
                    }
                }
                Msg::LauncherUpdated(result) => {
                    self.job = None;
                    match result.and_then(|exe| setup::launch(&exe, true)) {
                        Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                        Err(e) => self.message = Some((true, format!("Updating the launcher failed: {e}"))),
                    }
                }
            }
        }
    }

    /// First run of a downloaded copy.
    fn welcome(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let installed = setup::is_installed();
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                if let Some(t) = &self.icon {
                    ui.add(egui::Image::from_texture((t.id(), egui::vec2(96.0, 96.0))));
                }
                ui.add_space(8.0);
                ui.label(RichText::new("Open3D Launcher").size(26.0).strong());
                ui.label(RichText::new(format!("Version {VERSION}")).weak());
                ui.add_space(14.0);
                if installed {
                    ui.label("Open3D Launcher is already installed on this computer.");
                    ui.label(
                        RichText::new("Install this version over it, or open the installed one.")
                            .weak(),
                    );
                } else {
                    ui.label("Install the launcher on this computer to keep the Open3D apps for 3ds Max up to date.");
                    ui.label(
                        RichText::new(
                            "It goes in your user folder (no administrator rights), with a Start menu shortcut, \
                             and it shows in Windows' installed apps so you can remove it.",
                        )
                        .weak(),
                    );
                }
                ui.add_space(10.0);
                ui.add_enabled(
                    !self.setup_busy,
                    egui::Checkbox::new(&mut self.desktop_shortcut, "Add a desktop shortcut"),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    // Centre the row of buttons.
                    let width = if installed { 3.0 * 180.0 + 16.0 } else { 2.0 * 180.0 + 8.0 };
                    ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
                    let wide = |b: egui::Button<'static>| b.min_size(egui::vec2(180.0, 34.0));
                    let label = if installed { "Install this version" } else { "Install" };
                    if ui.add_enabled(!self.setup_busy, wide(primary(label))).clicked() {
                        let desktop = self.desktop_shortcut;
                        self.spawn_setup(ctx, move || Msg::SelfInstalled(setup::install_self(desktop)));
                    }
                    if installed
                        && ui
                            .add_enabled(!self.setup_busy, wide(secondary("Open the installed launcher")))
                            .clicked()
                    {
                        match setup::installed_exe().ok_or("LOCALAPPDATA is not set".to_string()).and_then(|e| setup::launch(&e, false)) {
                            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                            Err(e) => self.setup_error = Some(e),
                        }
                    }
                    if ui.add_enabled(!self.setup_busy, wide(secondary("Run without installing"))).clicked() {
                        self.screen = Screen::Main;
                    }
                });
                ui.add_space(10.0);
                if self.setup_busy {
                    ui.spinner();
                }
                if let Some(e) = &self.setup_error {
                    ui.label(RichText::new(e.as_str()).color(ERR));
                }
            });
        });
    }

    /// `--uninstall`, from Windows' installed apps.
    fn uninstall_screen(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| {
                if let Some(t) = &self.icon {
                    ui.add(egui::Image::from_texture((t.id(), egui::vec2(72.0, 72.0))));
                }
                ui.add_space(8.0);
                ui.label(RichText::new("Remove Open3D Launcher?").size(22.0).strong());
                ui.add_space(6.0);
                ui.label("Its shortcuts and its entry in Windows' installed apps are removed too.");
                ui.label(
                    RichText::new("The apps you installed with it stay in 3ds Max; remove those from the launcher first if you want them gone.")
                        .weak(),
                );
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.add_space(((ui.available_width() - 2.0 * 140.0 - 8.0) / 2.0).max(0.0));
                    if ui.add_enabled(!self.setup_busy, primary("Remove").min_size(egui::vec2(140.0, 34.0))).clicked() {
                        self.spawn_setup(ctx, || Msg::SelfUninstalled(setup::uninstall_self()));
                    }
                    if ui.add_enabled(!self.setup_busy, secondary("Cancel").min_size(egui::vec2(140.0, 34.0))).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                if let Some(e) = &self.setup_error {
                    ui.add_space(8.0);
                    ui.label(RichText::new(e.as_str()).color(ERR));
                }
            });
        });
    }

    fn done_screen(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, text: &str) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(80.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(text).size(16.0));
                ui.add_space(14.0);
                if ui.add(primary("Close")).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
    }

    fn start(&mut self, ctx: &egui::Context, app: AppEntry, verb: Verb) {
        if self.job.is_some() {
            return;
        }
        self.message = None;
        self.confirm_uninstall = None;
        self.job = Some(Job {
            name: app.name.clone(),
            verb,
            done: 0,
            total: None,
        });
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let plugins = self.opts.plugins.clone();
        let package = self.opts.catalog.join(&app.latest.url);
        std::thread::spawn(move || {
            let result = run_job(&app, verb, &plugins, &package, |done, total| {
                let _ = tx.send(Msg::Progress { done, total });
                ctx.request_repaint();
            });
            let _ = tx.send(Msg::Finished {
                name: app.name.clone(),
                verb,
                result,
            });
            ctx.request_repaint();
        });
    }
}

fn run_job(
    app: &AppEntry,
    verb: Verb,
    plugins: &std::path::Path,
    package: &catalog::Location,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<(), String> {
    let max_open = || Err("3ds Max is running. Close it and try again.".to_string());
    if install::max_running() {
        return max_open();
    }
    if verb == Verb::Uninstall {
        return install::uninstall(plugins, app);
    }
    let rel = &app.latest;
    let tmp = std::env::temp_dir().join(format!("open3d-{}-{}.zip", app.id, rel.version));
    let mut last = Instant::now() - Duration::from_secs(1);
    let hash = net::download(package, &tmp, |done, total| {
        if last.elapsed() > Duration::from_millis(80) || Some(done) == total {
            last = Instant::now();
            progress(done, total);
        }
    });
    let result = (|| {
        let hash = hash?;
        if !hash.eq_ignore_ascii_case(&rel.sha256) {
            return Err("The download is damaged (its checksum does not match the catalog).".to_string());
        }
        let size = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
        if rel.size > 0 && size != rel.size {
            return Err(format!(
                "The download has {size} bytes, the catalog says {}.",
                rel.size
            ));
        }
        if install::max_running() {
            return max_open();
        }
        install::install(plugins, app, &tmp, &rel.version)
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

enum Status {
    NotInstalled,
    UpdateAvailable,
    UpToDate,
    Newer,
}

fn status(app: &AppEntry, state: &AppState) -> Status {
    match &state.installed {
        None => Status::NotInstalled,
        Some(v) => match catalog::compare_versions(v, &app.latest.version) {
            Ordering::Less => Status::UpdateAvailable,
            Ordering::Equal => Status::UpToDate,
            Ordering::Greater => Status::Newer,
        },
    }
}

fn status_label(s: &Status) -> RichText {
    match s {
        Status::NotInstalled => RichText::new("Not installed").weak(),
        Status::UpdateAvailable => RichText::new("Update available").color(ACCENT),
        Status::UpToDate => RichText::new("Up to date").color(OK),
        Status::Newer => RichText::new("Newer than the catalog").weak(),
    }
}

fn primary(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text).color(Color32::BLACK).strong())
        .fill(ACCENT)
        .min_size(egui::vec2(110.0, 30.0))
}

fn secondary(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text)).min_size(egui::vec2(110.0, 30.0))
}

impl eframe::App for Launcher {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_messages(&ctx);
        match self.screen.clone() {
            Screen::Welcome => return self.welcome(ui, &ctx),
            Screen::Uninstall => return self.uninstall_screen(ui, &ctx),
            Screen::Done(text) => return self.done_screen(ui, &ctx, &text),
            Screen::Main => {}
        }

        egui::Panel::top("header").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Open3D").size(22.0).strong().color(ACCENT));
                ui.label(RichText::new(concat!("Launcher ", env!("CARGO_PKG_VERSION"))).weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let busy = self.loading || self.job.is_some();
                    if ui.add_enabled(!busy, secondary("Refresh")).clicked() {
                        self.message = None;
                        self.load_catalog(&ctx);
                    }
                });
            });
            ui.add_space(6.0);
        });

        if let Some(rel) = self.launcher_update() {
            let mut update = false;
            egui::Panel::top("launcher_update")
                .frame(
                    egui::Frame::new()
                        .fill(ACCENT.linear_multiply(0.18))
                        .inner_margin(8.0),
                )
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "Open3D Launcher {} is available (you have {VERSION}).",
                                rel.version
                            ))
                            .color(ACCENT),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let free = self.job.is_none() && !self.setup_busy;
                            if ui.add_enabled(free, primary("Update launcher")).clicked() {
                                update = true;
                            }
                        });
                    });
                });
            if update {
                self.start_launcher_update(&ctx, rel);
            }
        }

        if self.max_running {
            egui::Panel::top("max_banner")
                .frame(
                    egui::Frame::new()
                        .fill(WARN.linear_multiply(0.25))
                        .inner_margin(8.0),
                )
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("3ds Max is running. Close it to install, update or remove apps.")
                            .color(WARN),
                    );
                });
        }

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(4.0);
            if let Some(j) = &self.job {
                let label = match j.total {
                    Some(t) if t > 0 => format!(
                        "{} {}… {:.1} / {:.1} MB",
                        j.verb.doing(),
                        j.name,
                        j.done as f64 / 1e6,
                        t as f64 / 1e6
                    ),
                    _ => format!("{} {}…", j.verb.doing(), j.name),
                };
                let fraction = match j.total {
                    Some(t) if t > 0 => j.done as f32 / t as f32,
                    _ => 0.0,
                };
                ui.add(egui::ProgressBar::new(fraction).text(label).animate(true));
            } else if let Some((is_err, text)) = &self.message {
                ui.label(RichText::new(text).color(if *is_err { ERR } else { OK }));
            } else {
                ui.label(
                    RichText::new(format!("Apps install to {}", self.opts.plugins.display()))
                        .weak()
                        .small(),
                );
            }
            ui.add_space(4.0);
        });

        let Some(cat) = self.catalog.clone() else {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    if self.loading {
                        ui.spinner();
                        ui.label("Loading the catalog…");
                    } else if let Some(e) = &self.catalog_error {
                        ui.label(RichText::new("Could not load the catalog").size(17.0).color(ERR));
                        ui.add_space(4.0);
                        ui.label(e.as_str());
                        ui.label(RichText::new(self.opts.catalog.describe()).weak().small());
                        ui.add_space(8.0);
                        if ui.add(secondary("Try again")).clicked() {
                            self.load_catalog(&ctx);
                        }
                    }
                });
            });
            return;
        };

        if cat.apps.is_empty() {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.label("The catalog has no apps yet.");
            });
            return;
        }
        let selected = self.selected.min(cat.apps.len() - 1);

        egui::Panel::left("apps").exact_size(300.0).show(ui, |ui| {
            ui.add_space(8.0);
            for (i, app) in cat.apps.iter().enumerate() {
                let state = self.states.get(i);
                let is_sel = i == selected;
                let frame = egui::Frame::group(ui.style())
                    .fill(if is_sel {
                        ui.visuals().selection.bg_fill.linear_multiply(0.35)
                    } else {
                        Color32::TRANSPARENT
                    })
                    .inner_margin(10.0)
                    .corner_radius(6.0);
                let resp = frame
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(&app.name).size(16.0).strong());
                        if !app.summary.is_empty() {
                            ui.label(RichText::new(&app.summary).weak());
                        }
                        if let Some(s) = state {
                            ui.horizontal(|ui| {
                                ui.label(status_label(&status(app, s)));
                                if let Some(v) = &s.installed {
                                    ui.label(RichText::new(format!("v{v}")).weak().small());
                                }
                            });
                        }
                    })
                    .response
                    .interact(egui::Sense::click());
                if resp.clicked() {
                    self.selected = i;
                    self.confirm_uninstall = None;
                }
                ui.add_space(4.0);
            }
        });

        let app = &cat.apps[selected];
        let state = self.states.get(selected);
        let mut action: Option<Verb> = None;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(8.0);
            ui.label(RichText::new(&app.name).size(24.0).strong());
            if !app.summary.is_empty() {
                ui.label(&app.summary);
            }
            ui.add_space(8.0);
            egui::Grid::new("facts")
                .num_columns(2)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Installed").weak());
                    ui.label(
                        state
                            .and_then(|s| s.installed.clone())
                            .unwrap_or_else(|| "—".into()),
                    );
                    ui.end_row();
                    ui.label(RichText::new("Latest").weak());
                    let date = if app.latest.date.is_empty() {
                        String::new()
                    } else {
                        format!("  ({})", app.latest.date)
                    };
                    ui.label(format!("{}{date}", app.latest.version));
                    ui.end_row();
                    if let Some(m) = app.max_min {
                        ui.label(RichText::new("Needs").weak());
                        ui.label(format!("3ds Max {m} or newer"));
                        ui.end_row();
                    }
                });
            ui.add_space(10.0);

            let can_act = self.job.is_none() && !self.max_running;
            let st = state.map(|s| status(app, s)).unwrap_or(Status::NotInstalled);
            if self.confirm_uninstall.as_deref() == Some(app.id.as_str()) {
                ui.label(format!(
                    "Remove {} from 3ds Max? Its settings and your data are kept.",
                    app.name
                ));
                ui.horizontal(|ui| {
                    if ui.add_enabled(can_act, primary("Remove")).clicked() {
                        action = Some(Verb::Uninstall);
                    }
                    if ui.add(secondary("Cancel")).clicked() {
                        self.confirm_uninstall = None;
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    match st {
                        Status::NotInstalled => {
                            if ui.add_enabled(can_act, primary("Install")).clicked() {
                                action = Some(Verb::Install);
                            }
                        }
                        Status::UpdateAvailable => {
                            if ui.add_enabled(can_act, primary("Update")).clicked() {
                                action = Some(Verb::Update);
                            }
                        }
                        Status::UpToDate | Status::Newer => {
                            if ui.add_enabled(can_act, secondary("Reinstall")).clicked() {
                                action = Some(Verb::Reinstall);
                            }
                        }
                    }
                    if !matches!(st, Status::NotInstalled)
                        && ui.add_enabled(can_act, secondary("Remove")).clicked()
                    {
                        self.confirm_uninstall = Some(app.id.clone());
                    }
                });
            }

            if let Some(s) = state
                && !s.other_copies.is_empty()
            {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "Also found {} in the plugins folder. 3ds Max loads only one copy of {}: remove the \
                             other one (a development copy?) to use the version installed here.",
                        s.other_copies.join(", "),
                        app.name
                    ))
                    .color(WARN),
                );
            }

            ui.add_space(12.0);
            ui.separator();
            ui.label(
                RichText::new(format!("What's new in {}", app.latest.version))
                    .size(16.0)
                    .strong(),
            );
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if app.latest.notes.trim().is_empty() {
                        ui.label(RichText::new("No notes for this version.").weak());
                    } else {
                        notes::show(ui, &app.latest.notes);
                    }
                });
        });

        if let Some(verb) = action {
            self.start(&ctx, app.clone(), verb);
        }
    }
}
