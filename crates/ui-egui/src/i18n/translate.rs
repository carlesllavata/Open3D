// SPDX-License-Identifier: GPL-3.0-or-later
//! Looking strings up in the language the UI is drawn in.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::lang::{LANGUAGES, Lang};

/// Index into [`LANGUAGES`] of the language the UI is drawn in this frame.
static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// Set the UI language for drawing (the shell calls this once per frame from the preference), so
/// widgets can translate without every call site carrying a language around.
pub fn set_current(lang: Lang) {
    CURRENT.store(lang.index(), Ordering::Relaxed);
}

/// The language the UI is drawn in.
pub fn current() -> Lang {
    LANGUAGES.get(CURRENT.load(Ordering::Relaxed)).map_or(Lang::EN, Lang)
}

/// Does `lang` have a catalog entry for this plain string? (English never does: it is the source.)
pub fn has(lang: Lang, s: &str) -> bool {
    lang.catalog().plain(s).is_some()
}

/// Translate an English UI string into the current language ([`tr`] with [`current`]).
pub fn t(s: &str) -> &str {
    tr(current(), s)
}

/// Translate an English UI string; unknown strings come back unchanged.
pub fn tr(lang: Lang, s: &str) -> &str {
    lang.catalog().plain(s).unwrap_or(s)
}

/// Like [`tr`], for an English string that needs a disambiguating `context`.
pub fn tr_ctx<'a>(lang: Lang, context: &str, s: &'a str) -> &'a str {
    lang.catalog().contextual(context, s).unwrap_or_else(|| tr(lang, s))
}

/// Fill `{name}` placeholders in one pass. Values and unknown placeholders stay as written.
pub fn fmt(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some((before, after)) = rest.split_once('{') {
        out.push_str(before);
        let filled = after.split_once('}').and_then(|(name, tail)| args.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v, tail)));
        if let Some((value, tail)) = filled {
            out.push_str(value);
            rest = tail;
        } else {
            out.push('{');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// A plural-aware string: `one`/`other` are the English forms (with `{n}` where the count goes),
/// and together (`one|other`) the key of the language's `@plural` row, which holds as many forms
/// as the language has. Without a row, English: `one` for 1, `other` for everything else.
pub fn trn(lang: Lang, n: u64, one: &str, other: &str) -> String {
    let idx = (lang.0.plural)(n);
    let english = if n == 1 { one } else { other };
    let text = lang.catalog().plural(&format!("{one}|{other}"), idx).unwrap_or(english);
    fmt(text, &[("n", &n.to_string())])
}

/// [`trn`] in the current language.
pub fn tn(n: u64, one: &str, other: &str) -> String {
    trn(current(), n, one, other)
}

/// Translate a status or error message for display (see the module docs). Unknown messages, and
/// the parts of a message a template's placeholders stand for (names, versions, reasons), stay
/// as they are.
pub fn message(lang: Lang, s: &str) -> String {
    lang.catalog().message(s).unwrap_or_else(|| s.to_string())
}

/// [`message`] in the current language, remembering the last answer: the status bar asks for the
/// same message every frame.
pub fn msg(s: &str) -> String {
    static LAST: std::sync::Mutex<Option<(&str, String, String)>> = std::sync::Mutex::new(None);
    let lang = current();
    let mut last = LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((code, src, out)) = last.as_ref()
        && *code == lang.code()
        && src == s
    {
        return out.clone();
    }
    let out = message(lang, s);
    *last = Some((lang.code(), s.to_string(), out.clone()));
    out
}
