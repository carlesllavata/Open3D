// SPDX-License-Identifier: GPL-3.0-or-later
// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod catalog;
mod install;
mod net;
mod notes;
mod setup;
mod ui;

use catalog::Location;
use std::path::PathBuf;

const USAGE: &str = "Open3D Launcher

Usage: open3d-launcher [--catalog <path|url>] [--plugins-dir <folder>] [--portable] [--uninstall]

  --catalog      catalog.json to read (default: the public Open3D catalog)
  --plugins-dir  where bundles are installed (default: %APPDATA%\\Autodesk\\ApplicationPlugins);
                 meant for testing
  --portable     run without offering to install the launcher
  --install      install the launcher without showing a window (Start menu and desktop shortcuts)
  --uninstall    remove the installed launcher (what Windows' installed apps runs);
                 add --quiet to do it without asking";

pub const ICON_PNG: &[u8] = include_bytes!("../assets/icon-256.png");

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    /// Normal start; offers to install the launcher when it isn't the installed copy.
    Normal,
    /// Straight to the app list (`--portable`, or a development run with a custom catalog / folder).
    Portable,
    Uninstall,
    /// `--install`: no window; the exit code says whether it worked.
    SilentInstall,
    /// `--uninstall --quiet`.
    SilentUninstall,
}

pub struct Options {
    pub catalog: Location,
    pub plugins: PathBuf,
    pub mode: StartMode,
}

fn parse_args() -> Result<Options, String> {
    let mut catalog = Location::parse(catalog::DEFAULT_CATALOG);
    let mut plugins = None;
    let mut mode = StartMode::Normal;
    let mut custom = false;
    let mut quiet = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--catalog" => {
                catalog = Location::parse(&args.next().ok_or("--catalog needs a value")?);
                custom = true;
            }
            "--plugins-dir" => {
                plugins = Some(PathBuf::from(args.next().ok_or("--plugins-dir needs a value")?));
                custom = true;
            }
            "--portable" => mode = StartMode::Portable,
            "--install" => mode = StartMode::SilentInstall,
            "--uninstall" => mode = StartMode::Uninstall,
            "--quiet" => quiet = true,
            "-h" | "--help" => return Err(USAGE.to_string()),
            other => return Err(format!("Unknown option {other}\n\n{USAGE}")),
        }
    }
    if custom && mode == StartMode::Normal {
        mode = StartMode::Portable;
    }
    if quiet && mode == StartMode::Uninstall {
        mode = StartMode::SilentUninstall;
    }
    // A relative catalog path is read from the current folder; keep it absolute so relative package
    // paths inside it resolve the same way later.
    if let Location::File(p) = &catalog
        && p.is_relative()
        && let Ok(cwd) = std::env::current_dir()
    {
        catalog = Location::File(cwd.join(p));
    }
    let plugins = match plugins {
        Some(p) => p,
        None => install::default_plugins_dir().ok_or("APPDATA is not set")?,
    };
    Ok(Options {
        catalog,
        plugins,
        mode,
    })
}

fn main() -> eframe::Result {
    let options = match parse_args() {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    let silent = match options.mode {
        StartMode::SilentInstall => Some(setup::install_self(true).map(|_| ())),
        StartMode::SilentUninstall => Some(setup::uninstall_self()),
        _ => None,
    };
    if let Some(result) = silent {
        if let Err(e) = &result {
            eprintln!("{e}");
        }
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }
    setup::clean_leftovers();
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("Open3D Launcher")
        .with_inner_size([960.0, 620.0])
        .with_min_inner_size([720.0, 460.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(ICON_PNG) {
        viewport = viewport.with_icon(icon);
    }
    let native = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Open3D Launcher",
        native,
        Box::new(move |cc| Ok(Box::new(ui::Launcher::new(cc, options)))),
    )
}
