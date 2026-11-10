//! A small Markdown renderer for release notes: headings, bullets (nested), paragraphs, **bold**, `code`,
//! [links](url). Lines that continue a bullet or a paragraph (wrapped in the CHANGELOG) are joined to it.

use eframe::egui::{self, Color32, FontId, TextFormat, text::LayoutJob};

#[derive(Debug, PartialEq)]
enum Block {
    Heading(usize, String),
    Paragraph(String),
    /// Nesting depth (0 = top level) and text.
    Bullet(usize, String),
    Gap,
}

fn parse(markdown: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    for line in markdown.lines() {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim();
        if t.is_empty() {
            if !matches!(blocks.last(), None | Some(Block::Gap)) {
                blocks.push(Block::Gap);
            }
            continue;
        }
        let hashes = t.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            blocks.push(Block::Heading(hashes, t[hashes..].trim().to_string()));
            continue;
        }
        if let Some(text) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            blocks.push(Block::Bullet(indent / 2, text.trim().to_string()));
            continue;
        }
        match blocks.last_mut() {
            Some(Block::Paragraph(p)) | Some(Block::Bullet(_, p)) => {
                p.push(' ');
                p.push_str(t);
            }
            _ => blocks.push(Block::Paragraph(t.to_string())),
        }
    }
    blocks
}

pub fn show(ui: &mut egui::Ui, markdown: &str) {
    for block in parse(markdown) {
        match block {
            Block::Gap => ui.add_space(4.0),
            Block::Heading(level, text) => {
                let size = match level {
                    1 => 20.0,
                    2 => 17.0,
                    _ => 15.0,
                };
                ui.add_space(6.0);
                ui.label(egui::RichText::new(text).size(size).strong());
            }
            Block::Paragraph(text) => {
                let job = inline(ui, &text);
                ui.add(egui::Label::new(job).wrap());
            }
            Block::Bullet(depth, text) => {
                ui.horizontal_top(|ui| {
                    ui.add_space(4.0 + depth as f32 * 18.0);
                    ui.label("•");
                    let job = inline(ui, &text);
                    ui.add(egui::Label::new(job).wrap());
                });
            }
        }
    }
}

fn inline(ui: &egui::Ui, text: &str) -> LayoutJob {
    let body = FontId::proportional(14.0);
    let mono = FontId::monospace(13.0);
    let normal = ui.visuals().text_color();
    let strong = ui.visuals().strong_text_color();
    let link = ui.visuals().hyperlink_color;
    let mut job = LayoutJob::default();
    let fmt = |font: &FontId, color: Color32| TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };

    let mut rest = text;
    let mut bold = false;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("**") {
            bold = !bold;
            rest = r;
            continue;
        }
        if let Some(r) = rest.strip_prefix('`')
            && let Some(end) = r.find('`')
        {
            job.append(&r[..end], 0.0, fmt(&mono, strong));
            rest = &r[end + 1..];
            continue;
        }
        if let Some(r) = rest.strip_prefix('[')
            && let Some(close) = r.find("](")
            && let Some(end) = r[close + 2..].find(')')
        {
            job.append(&r[..close], 0.0, fmt(&body, link));
            rest = &r[close + 2 + end + 1..];
            continue;
        }
        // Plain text up to the next marker (skip the first char so a lone marker still advances).
        let first = rest.chars().next().map_or(1, char::len_utf8);
        let next = rest[first..]
            .find(['*', '`', '['])
            .map(|i| i + first)
            .unwrap_or(rest.len());
        job.append(&rest[..next], 0.0, fmt(&body, if bold { strong } else { normal }));
        rest = &rest[next..];
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_wrapped_lines_to_their_bullet() {
        let md = "Intro line one\nline two.\n\n- **First.** It wraps\n  onto a second line.\n  - Nested\n    item.\n- Second";
        assert_eq!(
            parse(md),
            vec![
                Block::Paragraph("Intro line one line two.".into()),
                Block::Gap,
                Block::Bullet(0, "**First.** It wraps onto a second line.".into()),
                Block::Bullet(1, "Nested item.".into()),
                Block::Bullet(0, "Second".into()),
            ]
        );
    }

    #[test]
    fn headings_need_a_space() {
        assert_eq!(parse("## 0.5.0"), vec![Block::Heading(2, "0.5.0".into())]);
        assert_eq!(parse("#hashtag"), vec![Block::Paragraph("#hashtag".into())]);
    }
}
