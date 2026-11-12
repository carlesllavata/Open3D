// SPDX-License-Identifier: GPL-3.0-or-later
//! Release notes: GitHub's generated Markdown, shown readably without a Markdown engine.

use egui::{RichText, Ui};

use crate::theme::Tokens;

/// One line of notes, classified.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Line<'a> {
    Heading(&'a str),
    /// A bullet: its text, and the pull request link GitHub appends (`… in https://…/pull/12`).
    Bullet(&'a str, Option<&'a str>),
    Text(&'a str),
    Blank,
}

pub(crate) fn classify(line: &str) -> Line<'_> {
    let l = line.trim();
    if l.is_empty() {
        return Line::Blank;
    }
    if let Some(h) = l.strip_prefix('#') {
        return Line::Heading(h.trim_start_matches('#').trim());
    }
    if let Some(b) = l.strip_prefix("* ").or_else(|| l.strip_prefix("- ")) {
        if let Some((text, url)) = b.rsplit_once(" in https://")
            && !url.contains(' ')
        {
            let start = b.len() - url.len() - "https://".len();
            return Line::Bullet(text, b.get(start..));
        }
        return Line::Bullet(b, None);
    }
    Line::Text(l)
}

/// The notes as Markdown reads them: a line that only carries on the previous bullet or paragraph
/// (the source wraps long lines) is joined to it, so a wrapped change stays one bullet.
pub(crate) fn logical_lines(notes: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut open = false;
    for raw in notes.lines() {
        match (classify(raw), out.last_mut()) {
            (Line::Text(text), Some(last)) if open && !text.starts_with("**Full Changelog**") => {
                last.push(' ');
                last.push_str(text);
            }
            (line, _) => {
                open = matches!(line, Line::Bullet(..) | Line::Text(_));
                out.push(raw.trim_end().to_string());
            }
        }
    }
    out
}

/// `text` cut at its `**bold**` marks: each piece and whether it is bold. As in Markdown, the bold
/// text can't start or end with a space; a `**` without such a partner stays as written.
pub(crate) fn spans(text: &str) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    // `plain`: where the current plain piece starts; `at`: where to look for the next mark.
    let (mut plain, mut at) = (0, 0);
    while let Some(found) = text[at..].find("**") {
        let open = at + found;
        let inner = open + 2;
        match text[inner..].find("**").map(|n| inner + n) {
            Some(close) if is_bold(&text[inner..close]) => {
                if open > plain {
                    out.push((&text[plain..open], false));
                }
                out.push((&text[inner..close], true));
                plain = close + 2;
                at = plain;
            }
            _ => at = inner,
        }
    }
    if plain < text.len() {
        out.push((&text[plain..], false));
    }
    out
}

fn is_bold(inner: &str) -> bool {
    !inner.is_empty() && !inner.starts_with(char::is_whitespace) && !inner.ends_with(char::is_whitespace)
}

/// A label with the `**bold**` parts of `text` in the semibold face, after `bullet` if given (in the
/// same text, so both sit on one baseline).
fn rich_label(ui: &mut Ui, bullet: Option<egui::Color32>, text: &str, color: egui::Color32) {
    let size = egui::TextStyle::Body.resolve(ui.style()).size;
    let mut job = egui::text::LayoutJob::default();
    if let Some(bullet) = bullet {
        job.append("•  ", 0.0, egui::TextFormat { font_id: egui::FontId::proportional(size), color: bullet, ..Default::default() });
    }
    for (piece, bold) in spans(text) {
        let family = if bold { crate::theme::semibold() } else { egui::FontFamily::Proportional };
        job.append(piece, 0.0, egui::TextFormat { font_id: egui::FontId::new(size, family), color, ..Default::default() });
    }
    ui.label(job);
}

/// How many changes show before the rest folds away.
const SHOWN: usize = 6;

pub(crate) fn show(ui: &mut Ui, notes: &str) {
    show_scaled(ui, notes, 1.0);
}

/// The notes at `scale` times their usual size (the notes dialog's own zoom).
pub(crate) fn show_scaled(ui: &mut Ui, notes: &str, scale: f32) {
    ui.scope(|ui| {
        for font in ui.style_mut().text_styles.values_mut() {
            font.size *= scale;
        }
        show_lines(ui, notes, scale);
    });
}

/// The notes, short: the first few changes in full, the rest behind "Show N more changes", and the
/// link to the full changelog always in view.
fn show_lines(ui: &mut Ui, notes: &str, scale: f32) {
    let t = Tokens::get(ui.ctx());
    if notes.trim().is_empty() {
        ui.label(RichText::new(tl!("No notes were published with this release.")).color(t.muted));
        return;
    }
    let logical = logical_lines(notes);
    let (changelog, lines): (Vec<Line<'_>>, Vec<Line<'_>>) = logical.iter().map(|l| classify(l)).partition(|l| full_changelog(l).is_some());
    let bullets = lines.iter().filter(|l| matches!(l, Line::Bullet(..))).count();
    let (head, rest) = lines.split_at(fold_at(&lines).unwrap_or(lines.len()));
    for line in head {
        draw(ui, line, &t, scale);
    }
    if !rest.is_empty() {
        let label = crate::i18n::fmt(tl!("Show {n} more changes"), &[("n", &bullets.saturating_sub(SHOWN).to_string())]);
        egui::CollapsingHeader::new(label).id_salt(ui.scope_id().with(("more-notes", notes.len()))).show(ui, |ui| {
            for line in rest {
                draw(ui, line, &t, scale);
            }
        });
    }
    for line in &changelog {
        if let Some(url) = full_changelog(line) {
            ui.add_space(6.0);
            ui.hyperlink_to(tl!("Full changelog"), url);
        }
    }
}

/// Where a long list folds: just before its (SHOWN + 1)th change. A short list is shown whole.
fn fold_at(lines: &[Line<'_>]) -> Option<usize> {
    let bullets = lines.iter().filter(|l| matches!(l, Line::Bullet(..))).count();
    if bullets <= SHOWN + 1 {
        return None;
    }
    lines.iter().enumerate().filter(|(_, l)| matches!(l, Line::Bullet(..))).nth(SHOWN).map(|(i, _)| i)
}

/// The address on a "**Full Changelog**: …" line, when it is a safe web link.
fn full_changelog<'a>(line: &Line<'a>) -> Option<&'a str> {
    let Line::Text(text) = line else { return None };
    text.strip_prefix("**Full Changelog**: ").map(str::trim).filter(|url| open3d_launcher_core::release::safe_web_link(url))
}

fn draw(ui: &mut Ui, line: &Line<'_>, t: &Tokens, scale: f32) {
    match line {
        Line::Heading(h) => {
            ui.add_space(6.0);
            ui.label(crate::widgets::heading(h, 16.0 * scale, t.ink_strong));
        }
        Line::Bullet(text, link) => {
            // The author's handle is noise in a list of what changed.
            let text = text.rsplit_once(" by @").map_or(*text, |(change, _)| change);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                rich_label(ui, Some(t.faint), text, t.ink);
                if let Some(url) = link.filter(|u| open3d_launcher_core::release::safe_web_link(u)) {
                    let short = url.rsplit('/').next().map_or_else(|| tl!("link").to_string(), |n| format!("#{n}"));
                    ui.hyperlink_to(RichText::new(short).size(12.0 * scale), url);
                }
            });
        }
        Line::Text(text) => {
            rich_label(ui, None, text, t.muted);
        }
        Line::Blank => {
            ui.add_space(2.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_githubs_generated_notes() {
        assert_eq!(classify("## What's Changed"), Line::Heading("What's Changed"));
        assert_eq!(
            classify("* Add the Patch tool (#213) by @a in https://github.com/carlesllavata/phototool/pull/381"),
            Line::Bullet("Add the Patch tool (#213) by @a", Some("https://github.com/carlesllavata/phototool/pull/381"))
        );
        assert_eq!(classify("- plain bullet"), Line::Bullet("plain bullet", None));
        assert_eq!(classify("   "), Line::Blank);
        assert_eq!(classify("hello"), Line::Text("hello"));
    }

    #[test]
    fn bold_marks_become_bold_pieces() {
        assert_eq!(spans("**Fast passes.** Masks render in seconds."), vec![("Fast passes.", true), (" Masks render in seconds.", false)]);
        assert_eq!(spans("a **b** c **d**"), vec![("a ", false), ("b", true), (" c ", false), ("d", true)]);
        assert_eq!(spans("no marks"), vec![("no marks", false)]);
        // An unpaired mark is left as written.
        assert_eq!(spans("2 ** 3 and **x"), vec![("2 ** 3 and **x", false)]);
        assert_eq!(spans("**a** then **b"), vec![("a", true), (" then **b", false)]);
        assert!(spans("").is_empty());
    }

    #[test]
    fn wrapped_bullets_and_paragraphs_are_joined() {
        let notes = "Intro that\nwraps.\n\n- **One.** First\n  line wraps.\n- Two\n\n**Full Changelog**: https://github.com/a/b/compare/v1...v2";
        assert_eq!(
            logical_lines(notes),
            vec!["Intro that wraps.", "", "- **One.** First line wraps.", "- Two", "", "**Full Changelog**: https://github.com/a/b/compare/v1...v2"]
        );
        // A heading never takes the next line in.
        assert_eq!(logical_lines("## Title\ntext"), vec!["## Title", "text"]);
    }

    #[test]
    fn a_long_list_folds_after_six_changes_and_a_short_one_does_not() {
        let many: Vec<Line> = std::iter::once(Line::Heading("What's Changed")).chain((0..10).map(|_| Line::Bullet("change", None))).collect();
        // The heading, then six bullets: the seventh bullet (index 7) is the first one folded.
        assert_eq!(fold_at(&many), Some(7));
        let few: Vec<Line> = (0..7).map(|_| Line::Bullet("change", None)).collect();
        assert_eq!(fold_at(&few), None);
        assert_eq!(fold_at(&[]), None);
    }
}
