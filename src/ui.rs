//! The launcher window. All slow work (catalog, downloads, file moves, checking for 3ds Max) runs on
//! background threads that report back through a channel.

use crate::catalog::{self, AppEntry, Catalog};
use crate::{Options, install, net, notes};
use eframe::egui::{self, Color32, RichText};
use std::cmp::Ordering;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

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
}

impl Launcher {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Options) -> Self {
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
        };
        me.load_catalog(&ctx);
        me
    }

    fn load_catalog(&mut self, ctx: &egui::Context) {
        self.loading = true;
        self.catalog_error = None;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let loc = self.opts.catalog.clone();
        std::thread::spawn(move || {
            let result = net::read_text(&loc).and_then(|t| catalog::parse(&t));
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

    fn handle_messages(&mut self) {
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
            }
        }
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
        self.handle_messages();
        let ctx = ui.ctx().clone();

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
