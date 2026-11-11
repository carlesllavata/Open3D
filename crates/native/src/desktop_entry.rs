// SPDX-License-Identifier: GPL-3.0-or-later
//! Menu entries (freedesktop `.desktop` files) for AppImages the launcher installs.
//!
//! An AppImage installs nothing by itself, so without an entry the app is missing from the
//! desktop's menu and, on Wayland, the dock can't match the window's app id to an icon. The
//! launcher writes `<data>/applications/io.github.carlesllavata.<app>.desktop` (the id each app's window
//! reports) and marks it as its own, so it only ever replaces or removes entries it wrote.
//!
//! The text is built by pure functions, tested on every platform; only Linux writes the files.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::Path;

use open3d_launcher_core::SuiteApp;

/// The key that marks an entry as written by the launcher.
pub const MARKER: &str = "X-Open3D-Launcher=true";

/// The menu categories and the window class each app's own `.desktop` file declares.
pub fn meta(app: &SuiteApp) -> (&'static str, Option<&'static str>) {
    if let Some(meta) = open3d_launcher_core::hosted_catalog::metadata(app.id) {
        return (meta.desktop_categories.as_str(), Some(meta.startup_wm_class.as_str()));
    }
    match app.id {
        "phototool" => ("Graphics;2DGraphics;RasterGraphics;Photography;", Some("phototool")),
        "pdftool" => ("Office;Viewer;", Some("pdftool")),
        "lighttool" => ("Graphics;2DGraphics;RasterGraphics;Photography;", None),
        "filmtool" => ("AudioVideo;Video;AudioVideoEditing;", None),
        "designtool" => ("Graphics;Publishing;", None),
        "vectortool" => ("Graphics;2DGraphics;VectorGraphics;", None),
        "effecttool" => ("AudioVideo;Video;Graphics;2DGraphics;", None),
        "anitool" => ("Graphics;2DGraphics;VectorGraphics;AudioVideo;", Some("anitool")),
        "wordtool" => ("Office;WordProcessor;", None),
        "decktool" => ("Office;Presentation;", None),
        "gridtool" => ("Office;Spreadsheet;", None),
        "cadtool" => ("Graphics;Engineering;VectorGraphics;", None),
        "soundtool" => ("AudioVideo;Audio;Midi;Mixer;Sequencer;Recorder;", None),
        "figtool" => ("Graphics;2DGraphics;VectorGraphics;X-Design;", Some("figtool")),
        "texturetool" => ("Graphics;3DGraphics;", Some("texturetool")),
        "materialtool" => ("Graphics;3DGraphics;", Some("materialtool")),
        "anytool" => ("Development;Debugger;", None),
        _ => ("Graphics;", None),
    }
}

/// `path` as one quoted argument of an `Exec=` key, per the Desktop Entry spec: inside double
/// quotes `"`, `` ` ``, `$` and `\` take a backslash, then the string-value rule doubles every
/// backslash, and `%` (field codes) is written `%%`. `None` for paths that can't be written
/// safely (not UTF-8, holding a control character such as a line break, or holding `=`, which the
/// spec forbids in the executable name or path even when quoted).
pub fn exec_quote(path: &Path) -> Option<String> {
    let s = path.to_str()?;
    if s.chars().any(|c| c.is_control() || c == '=') {
        return None;
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' | '`' | '$' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '\\' => out.push_str("\\\\\\\\"),
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out.push('"');
    Some(out)
}

/// A localestring/string value: backslashes and line breaks escaped.
fn value(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

/// The entry for `app` installed as the AppImage `appimage`; `icon` when its icon is installed
/// under the app's id. `None` when the path can't be put in an `Exec=` line.
pub fn render(app: &SuiteApp, appimage: &Path, icon: bool) -> Option<String> {
    let exec = exec_quote(appimage)?;
    let (categories, wm_class) = meta(app);
    let mut text = String::from("[Desktop Entry]\nType=Application\n");
    text.push_str(&format!("Name={}\n", value(app.name)));
    text.push_str(&format!("Comment={}\n", value(app.tagline)));
    text.push_str(&format!("Exec={exec} %F\n"));
    if icon {
        text.push_str(&format!("Icon={}\n", app.bundle_id()));
    }
    text.push_str("Terminal=false\n");
    text.push_str(&format!("Categories={categories}\n"));
    text.push_str(&format!("StartupWMClass={}\n", wm_class.map_or_else(|| app.bundle_id(), str::to_string)));
    text.push_str(MARKER);
    text.push('\n');
    Some(text)
}

/// Whether the launcher wrote this entry.
pub fn is_ours(text: &str) -> bool {
    text.lines().any(|l| l.trim() == MARKER)
}

/// The program an entry's `Exec=` runs: its first argument, unquoted. Reads the launcher's own
/// quoting and the plain `Exec="<path>" %F` the apps' AppImages write for themselves.
pub fn exec_program(text: &str) -> Option<String> {
    let line = text.lines().find_map(|l| l.strip_prefix("Exec="))?;
    // String-value escapes first, then the quoting rule.
    let mut unescaped = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('s') => unescaped.push(' '),
                Some('n') => unescaped.push('\n'),
                Some('t') => unescaped.push('\t'),
                Some('r') => unescaped.push('\r'),
                Some(other) => unescaped.push(other),
                None => unescaped.push('\\'),
            }
        } else {
            unescaped.push(c);
        }
    }
    let mut out = String::new();
    let mut chars = unescaped.trim_start().chars();
    if unescaped.trim_start().starts_with('"') {
        chars.next();
        loop {
            match chars.next()? {
                '"' => break,
                '\\' => out.push(chars.next()?),
                c => out.push(c),
            }
        }
    } else {
        out.extend(chars.take_while(|c| !c.is_whitespace()));
    }
    Some(out.replace("%%", "%"))
}

#[cfg(test)]
mod tests {

    use super::*;

    fn app(id: &str) -> &'static SuiteApp {
        SuiteApp::by_id(id).unwrap()
    }
}
